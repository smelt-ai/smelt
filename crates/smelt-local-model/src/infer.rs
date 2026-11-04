//! 一次性同步推理：加载模型 → 生成 → 释放。函数返回时模型与上下文内存都已释放。

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Mutex;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;

use crate::error::{Error, Result};
use crate::title::{build_title_prompt_parts, extract_title};

/// llama.cpp 后端是进程级单例（`LlamaBackend::init` 重复调用会报错），
/// 推理串行化，每次调用独占并在结束时释放。
static BACKEND_LOCK: Mutex<()> = Mutex::new(());

/// 标题生成参数。默认值按 0.8B 小模型、短标题场景设定。
#[derive(Debug, Clone)]
pub struct TitleOptions {
    /// 上下文长度（token），决定 KV 缓存内存；1024 约占 100+ MB。
    pub n_ctx: u32,
    /// 输入先按字符截断到此长度，再按 token 截断以塞进上下文；两步都保留末尾（最新消息）。
    pub max_input_chars: usize,
    /// 最多生成的 token 数。
    pub max_new_tokens: u32,
    /// 结果标题的最大字符数。
    pub max_title_chars: usize,
    /// 推理线程数；`None` 取可用核数（上限 8）。
    pub threads: Option<u32>,
}

impl Default for TitleOptions {
    fn default() -> Self {
        Self {
            n_ctx: 1024,
            max_input_chars: 4000,
            max_new_tokens: 32,
            max_title_chars: 30,
            threads: None,
        }
    }
}

/// 加载 `model_path` 的 GGUF 模型，为 `input` 生成一个短标题，然后卸载模型。
///
/// 阻塞调用（加载 + 推理约数百毫秒到数秒），须在后台线程执行；
/// 同进程内并发调用会排队。模型文件不存在时返回 [`Error::NotInstalled`]，不会自动下载。
pub fn generate_title(model_path: &Path, input: &str, opts: &TitleOptions) -> Result<String> {
    let content = keep_tail_chars(input, opts.max_input_chars);
    if content.is_empty() {
        return Err(Error::EmptyInput);
    }
    if !model_path.is_file() {
        return Err(Error::NotInstalled(model_path.to_path_buf()));
    }

    let _guard = BACKEND_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut backend = LlamaBackend::init().map_err(Error::backend)?;
    backend.void_logs();

    let model = LlamaModel::load_from_file(&backend, model_path, &LlamaModelParams::default())
        .map_err(Error::backend)?;

    let raw = run(&backend, &model, content, opts)?;
    // model、backend 在此按声明逆序 drop，释放全部内存。
    extract_title(&raw, opts.max_title_chars).ok_or(Error::NoTitle)
}

fn run(backend: &LlamaBackend, model: &LlamaModel, content: &str, opts: &TitleOptions) -> Result<String> {
    let vocab = model.vocab();
    let (prefix, user, suffix) = build_title_prompt_parts(content);
    // 模板部分解析特殊 token；用户内容按普通文本分词，防止注入模板标记。
    let prefix_tokens = vocab.tokenize(prefix.as_bytes(), false, true);
    let mut user_tokens = vocab.tokenize(user.as_bytes(), false, false);
    let suffix_tokens = vocab.tokenize(suffix.as_bytes(), false, true);

    let n_ctx = opts.n_ctx as usize;
    let fixed = prefix_tokens.len() + suffix_tokens.len() + opts.max_new_tokens as usize;
    if fixed >= n_ctx {
        return Err(Error::PromptTooLong {
            tokens: fixed,
            limit: n_ctx,
        });
    }
    // 输入按时间顺序排列，保留末尾：标题须反映最新的用户消息。
    keep_tail(&mut user_tokens, n_ctx - fixed);
    if user_tokens.is_empty() {
        return Err(Error::EmptyInput);
    }

    let prompt: Vec<LlamaToken> = prefix_tokens
        .into_iter()
        .chain(user_tokens)
        .chain(suffix_tokens)
        .collect();

    let threads = opts.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get().min(8) as u32)
            .unwrap_or(4)
    }) as i32;
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(opts.n_ctx))
        .with_n_batch(opts.n_ctx)
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    let mut ctx = model.new_context(backend, ctx_params).map_err(Error::backend)?;

    let mut batch = LlamaBatch::new(prompt.len().max(1), 1);
    let last = prompt.len() - 1;
    for (i, &tok) in prompt.iter().enumerate() {
        batch
            .add(tok, i as i32, &[0], i == last)
            .map_err(Error::backend)?;
    }
    ctx.decode(&mut batch).map_err(Error::backend)?;

    // 贪心解码：标题要稳定、可复现。
    let mut sampler = LlamaSampler::greedy();
    let mut pos = prompt.len() as i32;
    let mut out = Vec::new();
    for _ in 0..opts.max_new_tokens {
        let tok = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(tok);
        if vocab.is_eog(tok) {
            break;
        }
        out.extend(vocab.token_to_piece(tok, false, None));
        // 第一行已结束就够了。
        if let Some(start) = out.iter().position(|b| !b.is_ascii_whitespace())
            && out[start..].contains(&b'\n')
        {
            break;
        }
        batch.clear();
        batch.add(tok, pos, &[0], true).map_err(Error::backend)?;
        pos += 1;
        ctx.decode(&mut batch).map_err(Error::backend)?;
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// 去掉首尾空白后保留最后 `max_chars` 个字符（按 char 计，不切断 UTF-8），再去掉开头空白。
fn keep_tail_chars(input: &str, max_chars: usize) -> &str {
    let input = input.trim();
    if max_chars == 0 {
        return "";
    }
    match input.char_indices().rev().nth(max_chars - 1) {
        Some((idx, _)) => input[idx..].trim_start(),
        None => input,
    }
}

/// 只保留最后 `max` 个元素。
fn keep_tail<T>(items: &mut Vec<T>, max: usize) {
    if items.len() > max {
        items.drain(..items.len() - max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_tail_chars_keeps_latest_message() {
        let input = format!("{}\n最新：修复登录超时", "旧消息".repeat(100));
        assert_eq!(keep_tail_chars(&input, 9), "最新：修复登录超时");
    }

    #[test]
    fn keep_tail_chars_respects_unicode_boundaries() {
        assert_eq!(keep_tail_chars("a😀b中文", 3), "b中文");
        assert_eq!(keep_tail_chars("a😀b中文", 4), "😀b中文");
    }

    #[test]
    fn keep_tail_chars_zero_is_empty() {
        assert_eq!(keep_tail_chars("你好", 0), "");
    }

    #[test]
    fn keep_tail_chars_exact_and_short_unchanged() {
        assert_eq!(keep_tail_chars("  你好世界 ", 4), "你好世界");
        assert_eq!(keep_tail_chars("你好", 10), "你好");
    }

    #[test]
    fn keep_tail_chars_trims_cut_whitespace() {
        assert_eq!(keep_tail_chars("旧 新消息", 4), "新消息");
    }

    #[test]
    fn keep_tail_keeps_suffix_tokens() {
        let mut v = vec![1, 2, 3, 4, 5];
        keep_tail(&mut v, 2);
        assert_eq!(v, [4, 5]);
        let mut v = vec![1, 2];
        keep_tail(&mut v, 2);
        assert_eq!(v, [1, 2]);
        keep_tail(&mut v, 5);
        assert_eq!(v, [1, 2]);
        keep_tail(&mut v, 0);
        assert!(v.is_empty());
    }

    #[test]
    fn missing_model_returns_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.gguf");
        let err = generate_title(&path, "修复登录超时", &TitleOptions::default()).unwrap_err();
        assert!(matches!(err, Error::NotInstalled(p) if p == path));
    }
}
