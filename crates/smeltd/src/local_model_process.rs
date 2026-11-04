//! 本机模型独立进程的 PID 登记处，给 `resource.usage` 量它用。
//!
//! 约定（模型 worker 侧）：
//! - 子进程起来后调 [`set_running`]；
//! - 子进程结束时调 [`clear`]，并且要在回收（`wait`）它**之前**调：没回收的 PID
//!   系统不会复用，这样这里登记的 PID 永远不会指到别的进程上，资源页不会显示别人的数。
//!
//! 只是一个原子量：worker 不加锁，采样时读一次，不唤醒谁。

use std::sync::atomic::{AtomicU32, Ordering};
use smelt_core::control_api::{
    InferenceMean, LocalModelInference, LocalModelInferenceAverage, LocalModelInferenceStats,
};
use smelt_local_model::TitleModelSpec;

/// 0 表示没有模型进程。
static PID: AtomicU32 = AtomicU32::new(0);

/// 模型子进程已启动。
pub(crate) fn set_running(pid: u32) {
    PID.store(pid, Ordering::SeqCst);
}

/// 模型子进程 `pid` 即将被回收。只清自己登记的那个，旧进程迟到的清理不会抹掉新进程。
pub(crate) fn clear(pid: u32) {
    let _ = PID.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
}

/// 当前模型子进程的 PID；没在跑是 `None`。
pub(crate) fn current() -> Option<u32> {
    Some(PID.load(Ordering::SeqCst)).filter(|pid| *pid > 1)
}

/// 隐藏子命令名：`smeltd __local-model-title`，输入从 stdin 读，结果 JSON 写 stdout。
pub(crate) const TITLE_SUBCOMMAND: &str = "__local-model-title";

/// 输入上限（字节）。超出直接拒绝，不截断后静默推理。
const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct TitleReply {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// 子进程写结果前读的自身资源用量（见 [`smelt_sys::process_usage::LifetimeUsage`]）。
    /// 平台读不到的是 `None`；0 是真的 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cpu_time_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    peak_memory_bytes: Option<u64>,
}

/// 本次守护进程生命周期内成功推理的统计，只在内存里；采集失败不影响起标题。
#[derive(Default)]
struct InferenceStats {
    last: Option<LocalModelInference>,
    /// 按模型 id 排序。
    per_model: std::collections::BTreeMap<String, Accumulator>,
}

#[derive(Default)]
struct Accumulator {
    inferences: u64,
    wall_time_us: u128,
    cpu: (u64, u128),
    peak_memory: (u64, u128),
}

impl InferenceStats {
    fn record(&mut self, inference: LocalModelInference) {
        let acc = self.per_model.entry(inference.model.clone()).or_default();
        acc.inferences += 1;
        acc.wall_time_us += u128::from(inference.wall_time_us);
        if let Some(cpu) = inference.cpu_time_us {
            acc.cpu.0 += 1;
            acc.cpu.1 += u128::from(cpu);
        }
        if let Some(peak) = inference.peak_memory_bytes {
            acc.peak_memory.0 += 1;
            acc.peak_memory.1 += u128::from(peak);
        }
        self.last = Some(inference);
    }

    fn snapshot(&self) -> LocalModelInferenceStats {
        let mean = |(samples, sum): (u64, u128)| {
            (samples > 0).then(|| InferenceMean { samples, mean: sum as f64 / samples as f64 })
        };
        LocalModelInferenceStats {
            last: self.last.clone(),
            averages: self
                .per_model
                .iter()
                .map(|(model, acc)| LocalModelInferenceAverage {
                    model: model.clone(),
                    inferences: acc.inferences,
                    wall_time_us: acc.wall_time_us as f64 / acc.inferences.max(1) as f64,
                    cpu_time_us: mean(acc.cpu),
                    peak_memory_bytes: mean(acc.peak_memory),
                })
                .collect(),
        }
    }
}

static STATS: std::sync::LazyLock<std::sync::Mutex<InferenceStats>> =
    std::sync::LazyLock::new(Default::default);

/// 本次守护进程生命周期内的推理统计快照，给 `resource.usage` 带上。
pub(crate) fn inference_stats() -> LocalModelInferenceStats {
    STATS.lock().unwrap_or_else(|e| e.into_inner()).snapshot()
}

fn record_inference(inference: LocalModelInference) {
    STATS.lock().unwrap_or_else(|e| e.into_inner()).record(inference);
}

/// stdin 协议：4 字节小端长度 + UTF-8 payload（≤ [`MAX_INPUT_BYTES`]）。
/// 之后父进程**持有写端不关**，直到读完子进程 stdout；子进程在同一 stdin 上阻塞读到 EOF
/// 即知父进程已关闭或已死亡（内核在进程退出时关闭其全部句柄），随即退出。
/// 管道两端由操作系统保证，macOS / Linux / Windows 一致，无 PID 复用竞态，无轮询。
fn encode_frame(input: &str) -> Result<Vec<u8>, String> {
    let len = input.len();
    if len > MAX_INPUT_BYTES {
        return Err(format!("输入过长：{len} 字节，上限 {MAX_INPUT_BYTES}"));
    }
    let mut frame = Vec::with_capacity(4 + len);
    frame.extend_from_slice(&(len as u32).to_le_bytes());
    frame.extend_from_slice(input.as_bytes());
    Ok(frame)
}

#[derive(Debug, PartialEq, Eq)]
enum FrameError {
    /// 长度或 payload 没读全就 EOF：父进程已关闭写端/已死。
    Eof,
    Io(String),
    TooLong(u32),
    NotUtf8,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Eof => write!(f, "输入未读全即遇 EOF"),
            Self::Io(e) => write!(f, "读取输入失败：{e}"),
            Self::TooLong(n) => write!(f, "输入过长：{n} 字节，上限 {MAX_INPUT_BYTES}"),
            Self::NotUtf8 => write!(f, "输入不是合法 UTF-8"),
        }
    }
}

/// 读一帧。先校验长度再分配，超限不读 payload。
fn read_frame(r: &mut impl std::io::Read) -> Result<String, FrameError> {
    let map = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => FrameError::Eof,
        _ => FrameError::Io(e.to_string()),
    };
    let mut len = [0u8; 4];
    r.read_exact(&mut len).map_err(map)?;
    let len = u32::from_le_bytes(len);
    if len as usize > MAX_INPUT_BYTES {
        return Err(FrameError::TooLong(len));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).map_err(map)?;
    String::from_utf8(buf).map_err(|_| FrameError::NotUtf8)
}

#[derive(Debug, PartialEq, Eq)]
enum StdinEnd {
    /// 正常 EOF：父进程关闭写端或已退出。
    Eof,
    /// 帧之后又收到数据：协议违例，同样应退出。
    UnexpectedData,
    Error,
}

/// 阻塞读到 stdin 结束（事件驱动：read 在管道上阻塞，写端关闭时返回 0）。
fn wait_stdin_end(r: &mut impl std::io::Read) -> StdinEnd {
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) => return StdinEnd::Eof,
            Ok(_) => return StdinEnd::UnexpectedData,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return StdinEnd::Error,
        }
    }
}

/// 在独立子进程里跑本机模型起标题。用户文本只走 stdin，不进命令行。
/// 阻塞等待子进程退出；结束后先清 PID 登记再回收。
///
/// 顺序：写帧（不关写端）→ 读 stdout 到 EOF → 关写端 → 清登记 → wait。
/// 不会死锁：子进程先读完整帧才推理、才写 stdout，父写帧期间子在读；输出写完子即退出，
/// stdout EOF 不依赖父关闭 stdin。
///
/// 成功时把这次的墙钟耗时（启动子进程前到这里返回前）和子进程自报的 CPU / 内存峰值记进
/// 内存统计；失败不记。
pub(crate) fn generate_title(model: smelt_local_model::TitleModel, input: &str) -> Result<String, String> {
    let started = std::time::Instant::now();
    let reply = run_title_child(model, input)?;
    record_inference(LocalModelInference {
        model: model.id().to_string(),
        wall_time_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        cpu_time_us: reply.cpu_time_us,
        peak_memory_bytes: reply.peak_memory_bytes,
    });
    Ok(reply.title)
}

struct ChildTitle {
    title: String,
    cpu_time_us: Option<u64>,
    peak_memory_bytes: Option<u64>,
}

fn run_title_child(model: smelt_local_model::TitleModel, input: &str) -> Result<ChildTitle, String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let frame = encode_frame(input)?;
    // 必须使用守护启动时钉住的映像，启动名可能已指向升级后的另一版本。
    let exe = crate::daemon_executable_path()
        .map_err(|e| format!("找不到 smeltd 可执行文件：{e}"))?;
    let mut command = Command::new(exe);
    command
        .arg(TITLE_SUBCOMMAND)
        .arg(model.id())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(smelt_sys::process::CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("启动模型子进程失败：{e}"))?;
    let pid = child.id();
    set_running(pid);

    // 写端留在 `stdin` 里，直到读完 stdout 才 drop。
    let mut stdin = child.stdin.take();
    let write_result = match stdin.as_mut() {
        Some(w) => w.write_all(&frame).and_then(|_| w.flush()),
        None => Err(std::io::Error::other("子进程没有 stdin")),
    };
    let mut out = Vec::new();
    let read_result = match child.stdout.take() {
        Some(mut stdout) if write_result.is_ok() => stdout.read_to_end(&mut out).map(|_| ()),
        Some(_) => Ok(()),
        None => Err(std::io::Error::other("子进程没有 stdout")),
    };
    // 关闭写端：子进程若仍活着，监控线程读到 EOF 自行退出。
    drop(stdin);
    if write_result.is_err() || read_result.is_err() {
        let _ = child.kill();
    }
    // 先清登记再回收：回收前 PID 不会被系统复用。
    clear(pid);
    let status = child.wait().map_err(|e| format!("回收模型子进程失败：{e}"));

    write_result.map_err(|e| format!("向模型子进程写输入失败：{e}"))?;
    read_result.map_err(|e| format!("读取模型子进程输出失败：{e}"))?;
    let status = status?;
    let reply: TitleReply = serde_json::from_slice(&out)
        .map_err(|e| format!("模型子进程输出无法解析（{status}）：{e}"))?;
    match (reply.title, reply.error) {
        (_, Some(error)) => Err(error),
        (Some(title), None) if status.success() => Ok(ChildTitle {
            title,
            cpu_time_us: reply.cpu_time_us,
            peak_memory_bytes: reply.peak_memory_bytes,
        }),
        _ => Err(format!("模型子进程异常退出：{status}")),
    }
}

/// 子进程入口：从 stdin 读一帧，起监控线程等 stdin EOF，推理，JSON 写 stdout。返回退出码。
/// 父进程在本进程写出结果前关闭 stdin 或死亡 → 监控线程 `exit(4)`，不再写输出。
pub(crate) fn run_title_subcommand() -> i32 {
    use std::io::Write;
    let model_arg = std::env::args().nth(2);
    let model = parse_title_model(model_arg.as_deref());
    let input = match read_frame(&mut std::io::stdin().lock()) {
        Ok(input) => Ok(input),
        // 帧没读全：父进程已不在，没人收结果。
        Err(FrameError::Eof) => return 3,
        Err(e) => Err(e.to_string()),
    };
    // stdin 的锁已释放；监控线程接着读同一个 stdin（含其缓冲）。
    let spawned = std::thread::Builder::new()
        .name("smelt-model-parent-watch".into())
        .spawn(|| {
            let _ = wait_stdin_end(&mut std::io::stdin().lock());
            std::process::exit(4);
        });
    if spawned.is_err() {
        return 3;
    }
    let result = model.and_then(|model| input.and_then(|input| title_in_child(model, &input)));
    // 推理已结束，读本进程从启动到此刻的累计用量（含模型加载）。
    let usage = smelt_sys::process_usage::own_lifetime_usage();
    let (title, error) = match result {
        Ok(title) => (Some(title), None),
        Err(error) => (None, Some(error)),
    };
    let reply = TitleReply {
        title,
        error,
        cpu_time_us: usage.cpu_time_us,
        peak_memory_bytes: usage.peak_memory_bytes,
    };
    let code = if reply.error.is_some() { 1 } else { 0 };
    let Ok(json) = serde_json::to_vec(&reply) else {
        return 2;
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&json).and_then(|_| stdout.flush()).is_err() {
        return 2;
    }
    code
}

fn parse_title_model(id: Option<&str>) -> Result<smelt_local_model::TitleModel, String> {
    id.and_then(smelt_local_model::TitleModel::from_id)
        .ok_or_else(|| "未知或缺失本机标题模型".to_string())
}

fn title_in_child(model: smelt_local_model::TitleModel, input: &str) -> Result<String, String> {
    let dir = smelt_paths::smelt_home()
        .map(|home| home.join("models"))
        .ok_or_else(|| "找不到 smelt 主目录".to_string())?;
    let spec = model.spec();
    if !matches!(smelt_local_model::is_installed(&dir, spec), Ok(true)) {
        return Err("本机模型未安装".into());
    }
    let path = smelt_local_model::model_path(&dir, spec);
    smelt_local_model::generate_title(&path, input, &smelt_local_model::TitleOptions::default())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_late_clear_from_an_old_process_keeps_the_new_one() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_running(1000);
        set_running(2000);
        clear(1000);
        assert_eq!(current(), Some(2000));
        clear(2000);
        assert_eq!(current(), None);
    }

    use std::io::Cursor;

    fn inference(model: &str, wall: u64, cpu: Option<u64>, peak: Option<u64>) -> LocalModelInference {
        LocalModelInference {
            model: model.into(),
            wall_time_us: wall,
            cpu_time_us: cpu,
            peak_memory_bytes: peak,
        }
    }

    #[test]
    fn averages_count_each_field_only_where_it_has_data_and_split_by_model() {
        let mut stats = InferenceStats::default();
        assert_eq!(stats.snapshot(), LocalModelInferenceStats::default());
        stats.record(inference("qwen3-0.6b", 100, Some(0), None));
        stats.record(inference("qwen3-0.6b", 300, None, Some(1000)));
        stats.record(inference("qwen3.5-0.8b", 50, Some(40), Some(2000)));
        let snap = stats.snapshot();
        assert_eq!(snap.last, Some(inference("qwen3.5-0.8b", 50, Some(40), Some(2000))));
        assert_eq!(snap.averages.len(), 2);
        let a = &snap.averages[0];
        assert_eq!(a.model, "qwen3-0.6b");
        assert_eq!(a.inferences, 2);
        assert_eq!(a.wall_time_us, 200.0);
        // 0 是真数据，算一个样本。
        assert_eq!(a.cpu_time_us, Some(InferenceMean { samples: 1, mean: 0.0 }));
        assert_eq!(a.peak_memory_bytes, Some(InferenceMean { samples: 1, mean: 1000.0 }));
        let b = &snap.averages[1];
        assert_eq!((b.model.as_str(), b.inferences), ("qwen3.5-0.8b", 1));
        assert_eq!(b.cpu_time_us, Some(InferenceMean { samples: 1, mean: 40.0 }));
    }

    #[test]
    fn averages_without_any_cpu_or_memory_data_are_none() {
        let mut stats = InferenceStats::default();
        stats.record(inference("qwen3-0.6b", 10, None, None));
        let a = &stats.snapshot().averages[0];
        assert_eq!(a.cpu_time_us, None);
        assert_eq!(a.peak_memory_bytes, None);
    }

    #[test]
    fn reply_usage_fields_are_optional_and_keep_zero() {
        let old: TitleReply = serde_json::from_str(r#"{"title":"t"}"#).unwrap();
        assert_eq!((old.cpu_time_us, old.peak_memory_bytes), (None, None));
        let reply = TitleReply {
            title: Some("t".into()),
            error: None,
            cpu_time_us: Some(0),
            peak_memory_bytes: None,
        };
        let json = serde_json::to_value(&reply).unwrap();
        assert_eq!(json, serde_json::json!({"title": "t", "error": null, "cpu_time_us": 0}));
        let back: TitleReply = serde_json::from_value(json).unwrap();
        assert_eq!((back.cpu_time_us, back.peak_memory_bytes), (Some(0), None));
    }

    #[test]
    fn model_argument_is_required_and_must_be_known() {
        assert_eq!(parse_title_model(Some("qwen3.5-0.8b")), Ok(smelt_local_model::TitleModel::Qwen3_5_0_8B));
        assert!(parse_title_model(None).is_err());
        assert!(parse_title_model(Some("unknown")).is_err());
        assert!(parse_title_model(Some("qwen3-0.6b")).is_err());
    }

    #[test]
    fn frame_round_trips_and_leaves_stream_at_eof() {
        let frame = encode_frame("你好 world").unwrap();
        let mut r = Cursor::new(frame);
        assert_eq!(read_frame(&mut r).unwrap(), "你好 world");
        assert_eq!(wait_stdin_end(&mut r), StdinEnd::Eof);
    }

    #[test]
    fn empty_and_max_inputs_are_accepted() {
        let mut r = Cursor::new(encode_frame("").unwrap());
        assert_eq!(read_frame(&mut r).unwrap(), "");
        let max = "a".repeat(MAX_INPUT_BYTES);
        let mut r = Cursor::new(encode_frame(&max).unwrap());
        assert_eq!(read_frame(&mut r).unwrap(), max);
    }

    #[test]
    fn oversized_input_is_rejected_on_both_sides() {
        assert!(encode_frame(&"a".repeat(MAX_INPUT_BYTES + 1)).is_err());
        let len = MAX_INPUT_BYTES as u32 + 1;
        let mut r = Cursor::new(len.to_le_bytes().to_vec());
        assert_eq!(read_frame(&mut r), Err(FrameError::TooLong(len)));
    }

    #[test]
    fn parent_closing_before_full_frame_is_eof() {
        assert_eq!(read_frame(&mut Cursor::new(vec![])), Err(FrameError::Eof));
        assert_eq!(read_frame(&mut Cursor::new(vec![5, 0])), Err(FrameError::Eof));
        let mut short = 5u32.to_le_bytes().to_vec();
        short.extend_from_slice(b"ab");
        assert_eq!(read_frame(&mut Cursor::new(short)), Err(FrameError::Eof));
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let mut frame = 2u32.to_le_bytes().to_vec();
        frame.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(read_frame(&mut Cursor::new(frame)), Err(FrameError::NotUtf8));
    }

    #[test]
    fn trailing_bytes_after_frame_are_a_protocol_violation() {
        let mut frame = encode_frame("x").unwrap();
        frame.push(0);
        let mut r = Cursor::new(frame);
        assert_eq!(read_frame(&mut r).unwrap(), "x");
        assert_eq!(wait_stdin_end(&mut r), StdinEnd::UnexpectedData);
    }
}
