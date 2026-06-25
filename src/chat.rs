//! chat：基于已蒸馏的 instincts，提供交互式数字分身对话。
//! 支持调用飞书工具（function calling）：检查登录态、列会话、读消息。

use crate::db;
use crate::digest;
use crate::feishu::{discovery, gateway, messages};
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{self, BufRead, Write};

const API_URL: &str = "https://api.deepseek.com/chat/completions";
const MODEL: &str = "deepseek-chat";
const MAX_TOKENS: u32 = 4096;

// ── 工具定义 ──────────────────────────────────────────────────────────────────

fn tools_def() -> Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "feishu_check_auth",
                "description": "检查飞书登录态是否有效",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "feishu_list_chats",
                "description": "列出飞书最近的活跃会话（群聊和单聊），返回会话名称和 chat_id",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "feishu_get_messages",
                "description": "读取飞书指定会话的最近消息",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "chat_id": { "type": "string", "description": "会话 ID（chatId）" },
                        "count":   { "type": "integer", "description": "读取条数，默认 20" }
                    },
                    "required": ["chat_id"]
                }
            }
        }
    ])
}

// ── 流式响应解析 ──────────────────────────────────────────────────────────────

/// 流式一轮对话中累积的 tool call。
#[derive(Default)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

/// 一轮对话的结果。
enum TurnResult {
    /// 正常文本回复（已流式打印），附带完整文本供追加历史。
    Text(String),
    /// 模型要求调用工具：(assistant message json, tool calls)
    ToolCalls(Value, Vec<ToolCallAcc>),
}

/// 发送一轮请求，流式打印文本，返回 TurnResult。
async fn send_turn(
    client: &reqwest::Client,
    key: &str,
    system: &str,
    messages: &[Value],
) -> Result<TurnResult> {
    let mut all = vec![serde_json::json!({"role":"system","content":system})];
    all.extend_from_slice(messages);

    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "tools": tools_def(),
        "messages": all,
    });

    let mut resp = client
        .post(API_URL)
        .bearer_auth(key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("请求 DeepSeek API 失败")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("DeepSeek API 错误 {}: {}", status, body);
    }

    let mut text = String::new();
    // tool_calls 按 index 累积
    let mut tool_calls: Vec<ToolCallAcc> = Vec::new();
    let mut finish_reason = String::new();
    let mut buf = String::new();

    while let Some(chunk) = resp.chunk().await.context("读取流失败")? {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim().to_string();
            buf.drain(..=pos);
            let Some(data) = line.strip_prefix("data: ") else { continue };
            if data == "[DONE]" { break; }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };

            let choice = &v["choices"][0];

            // 收集 finish_reason
            if let Some(r) = choice["finish_reason"].as_str() {
                if !r.is_empty() && r != "null" {
                    finish_reason = r.to_string();
                }
            }

            let delta = &choice["delta"];

            // 文本 delta
            if let Some(t) = delta["content"].as_str() {
                print!("{t}");
                io::stdout().flush()?;
                text.push_str(t);
            }

            // tool_calls delta（按 index 累积）
            if let Some(tcs) = delta["tool_calls"].as_array() {
                for tc in tcs {
                    let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                    while tool_calls.len() <= idx {
                        tool_calls.push(ToolCallAcc::default());
                    }
                    let acc = &mut tool_calls[idx];
                    if let Some(id) = tc["id"].as_str() { acc.id = id.to_string(); }
                    if let Some(n) = tc["function"]["name"].as_str() { acc.name = n.to_string(); }
                    if let Some(a) = tc["function"]["arguments"].as_str() { acc.arguments.push_str(a); }
                }
            }
        }
    }

    if !text.is_empty() {
        println!();
    }

    if finish_reason == "tool_calls" && !tool_calls.is_empty() {
        // 构造 assistant message（含 tool_calls）供追加历史
        let tc_json: Vec<Value> = tool_calls.iter().map(|tc| serde_json::json!({
            "id": tc.id,
            "type": "function",
            "function": { "name": tc.name, "arguments": tc.arguments }
        })).collect();
        let assistant_msg = serde_json::json!({
            "role": "assistant",
            "content": if text.is_empty() { Value::Null } else { Value::String(text) },
            "tool_calls": tc_json,
        });
        Ok(TurnResult::ToolCalls(assistant_msg, tool_calls))
    } else {
        Ok(TurnResult::Text(text))
    }
}

// ── 工具执行 ──────────────────────────────────────────────────────────────────

async fn execute_tool(name: &str, args: &Value, session: &str) -> String {
    match name {
        "feishu_check_auth" => {
            if gateway::check_auth(session).await {
                "飞书登录态有效。".to_string()
            } else {
                "飞书登录态无效或已过期，需重新扫码（lark_cli.py login）。".to_string()
            }
        }
        "feishu_list_chats" => {
            match discovery::discover(session).await {
                Err(e) => format!("获取会话列表失败: {e}"),
                Ok(convs) => {
                    if convs.is_empty() {
                        return "未发现活跃会话。".to_string();
                    }
                    let mut out = format!("发现 {} 个活跃会话：\n", convs.len());
                    for c in &convs {
                        let kind = if c.is_group { "群" } else { "单聊" };
                        let name = if c.name.is_empty() { "(未命名单聊)".to_string() } else { c.name.clone() };
                        out.push_str(&format!("- [{kind}] {name}  chat_id={}\n", c.chat_id));
                    }
                    out
                }
            }
        }
        "feishu_get_messages" => {
            let chat_id = match args["chat_id"].as_str() {
                Some(id) => id,
                None => return "参数缺失：chat_id".to_string(),
            };
            let count = args["count"].as_u64().unwrap_or(20);
            match messages::fetch(session, chat_id, count).await {
                Err(e) => format!("读取消息失败: {e}"),
                Ok(msgs) => {
                    if msgs.is_empty() {
                        return "该会话无消息。".to_string();
                    }
                    let mut out = String::new();
                    for m in &msgs {
                        let t = chrono::DateTime::from_timestamp(m.timestamp as i64, 0)
                            .map(|d| d.format("%m-%d %H:%M").to_string())
                            .unwrap_or_default();
                        out.push_str(&format!("[{t}] {}: {}\n", m.sender_id, m.content));
                    }
                    out
                }
            }
        }
        _ => format!("未知工具: {name}"),
    }
}

// ── system prompt ─────────────────────────────────────────────────────────────

fn build_system_prompt() -> Result<String> {
    let conn = db::open()?;
    let instincts = db::list_by_confidence(&conn)?;

    let mut prompt = String::from(
        "你是这个人的数字分身。以下是从TA的真实行为中蒸馏出的编码直觉（instincts），\
置信度越高越核心，请以TA的视角和风格回答问题。\n\
如果不确定TA的立场，用第一人称表达推测，保持诚实。\n\
你可以调用飞书工具读取消息和会话，帮助回答与工作相关的问题。\n\n\
=== 我的 instincts ===\n",
    );
    if instincts.is_empty() {
        prompt.push_str("（尚无 instincts，先跑 `smelt digest` 提炼一次。）\n");
    } else {
        for it in &instincts {
            prompt.push_str(&format!(
                "[{:.2}] ({}) {}\n",
                it.confidence,
                it.domain.join("/"),
                it.content
            ));
        }
    }
    Ok(prompt)
}

// ── 主循环 ────────────────────────────────────────────────────────────────────

pub async fn run() -> Result<()> {
    let key = digest::api_key()?;
    let system = build_system_prompt()?;
    let client = reqwest::Client::new();

    // 尝试加载飞书 session（失败不阻断，工具调用时再报错）
    let session = gateway::load_session().ok();
    if session.is_some() {
        println!("飞书：已加载 session（可调用飞书工具）");
    } else {
        println!("飞书：未找到 session，飞书工具不可用（运行 lark_cli.py login）");
    }

    let conn = db::open()?;
    let instinct_count = db::list_by_confidence(&conn)?.len();
    println!("smelt chat — 和你的数字分身对话（Ctrl+D 或 exit 退出）");
    println!("{instinct_count} 条 instinct 已加载。\n");

    let mut messages: Vec<Value> = Vec::new();
    let stdin = io::stdin();

    loop {
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        match stdin.lock().read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => { eprintln!("读取输入失败: {e}"); break; }
        }
        let input = input.trim();
        if input.is_empty() { continue; }
        if matches!(input, "exit" | "quit" | "bye" | "q") { break; }

        messages.push(serde_json::json!({"role":"user","content":input}));

        // 工具调用循环：模型可能连续调用多个工具
        loop {
            match send_turn(&client, &key, &system, &messages).await {
                Err(e) => {
                    eprintln!("错误: {e:#}");
                    messages.pop(); // 移除失败的用户消息
                    break;
                }
                Ok(TurnResult::Text(reply)) => {
                    messages.push(serde_json::json!({"role":"assistant","content":reply}));
                    break;
                }
                Ok(TurnResult::ToolCalls(assistant_msg, calls)) => {
                    messages.push(assistant_msg);

                    // 执行所有工具，结果加回 messages
                    for tc in &calls {
                        let args: Value = serde_json::from_str(&tc.arguments)
                            .unwrap_or(Value::Object(Default::default()));

                        let sess = session.as_deref().unwrap_or("");
                        if session.is_none() {
                            let result_msg = serde_json::json!({
                                "role": "tool",
                                "tool_call_id": tc.id,
                                "content": "飞书未登录，无法执行此工具。"
                            });
                            messages.push(result_msg);
                            continue;
                        }

                        println!("\n[调用工具] {}({})", tc.name, tc.arguments.trim());
                        let result = execute_tool(&tc.name, &args, sess).await;
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tc.id,
                            "content": result,
                        }));
                    }
                    // 继续让模型处理工具结果
                }
            }
        }
    }

    println!("再见。");
    Ok(())
}
