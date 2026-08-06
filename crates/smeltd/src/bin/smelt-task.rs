//! `smelt-task`：agent 自循环塞任务的 CLI。让运行中的 agent（Claude Code / Codex 等）
//! 通过一条 shell 命令往 smelt 任务队列塞任务、查列表、声明完成——人和 agent 看到
//! 同一份 `~/.smelt/tasks.json`（由 smeltd 唯一持有，本工具经 socket op 读写）。
//!
//! 用法（agent 在 Bash 工具里直接跑）：
//! ```sh
//! smelt-task add --cwd . --body "下一条做 X"
//! smelt-task list --cwd .
//! smelt-task done <id>
//! ```
//!
//! 与 `smelt-notify` 的退出语义**相反**：hook 必须静默 exit 0；这里是显式命令，
//! 失败必须报错 exit 非 0，让 agent 看到错误。PTY 会话已注入 `SMELT_SOCK` env；
//! 没设时回退到 `~/.smelt/smeltd.sock`。

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use smelt_core::task::{
    Task, TaskKind, parse_local_datetime, task_prompt, title_from_prompt,
};

const USAGE: &str = "\
smelt-task — 向 smelt 任务队列塞/查任务（agent 自循环）

用法:
  smelt-task add --cwd <dir> --title <标题> --body <首包指令>
                 [--dep <id>]... [--auto-run on|off] [--retry-max <n>]
                 [--retry-delay <秒>] [--schedule 'YYYY-MM-DD HH:MM']
  smelt-task list [--cwd <dir>] [--all] [--json]
  smelt-task done <id>
  smelt-task remove <id>
  smelt-task show <id>
  smelt-task run <id>

选项:
  --json   list 输出机器可读 JSON
  --help   显示本帮助
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut sock = std::env::var("SMELT_SOCK").ok();
    if sock.is_none() {
        sock = Some(smelt_core::daemon_state::smeltd_sock_path().display().to_string());
    }
    match run(&args, sock.as_deref().unwrap_or("")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("smelt-task: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String], sock: &str) -> Result<(), String> {
    if args.is_empty() || args.contains(&"--help".to_string()) {
        print!("{USAGE}");
        return Ok(());
    }
    match args[0].as_str() {
        "add" => cmd_add(&args[1..], sock),
        "list" => cmd_list(&args[1..], sock),
        "done" => cmd_done(&args[1..], sock),
        "remove" => cmd_remove(&args[1..], sock),
        "show" => cmd_show(&args[1..], sock),
        "run" => cmd_run(&args[1..], sock),
        other => Err(format!("未知子命令：{other}\n{USAGE}")),
    }
}

/// 连 smeltd.sock 发一行 JSON op，读回一行响应，校验 `ok:true` 并返回整个响应。
fn request(sock: &str, payload: serde_json::Value) -> Result<serde_json::Value, String> {
    let mut stream = UnixStream::connect(sock).map_err(|e| format!("连不上 smeltd（{sock}）：{e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    writeln!(stream, "{payload}").map_err(|e| format!("写请求失败：{e}"))?;
    let _ = stream.shutdown(Shutdown::Write);
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|e| format!("读响应失败：{e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&response).map_err(|e| format!("响应非法：{e}"))?;
    if value["ok"].as_bool() != Some(true) {
        return Err(value["err"].as_str().unwrap_or("smeltd 拒绝了请求").to_string());
    }
    Ok(value)
}

fn opt(args: &[String], name: &str) -> Option<String> {
    let name = format!("--{name}");
    args.windows(2).find_map(|w| {
        if w[0] == name {
            Some(w[1].clone())
        } else {
            None
        }
    })
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a.as_str() == format!("--{name}"))
}

fn cmd_add(args: &[String], sock: &str) -> Result<(), String> {
    if has_flag(args, "launch") || has_flag(args, "channel") || has_flag(args, "agent") {
        return Err("任务暂不支持绑定 Agent；会在实际运行时按当前默认启动项执行".into());
    }
    let cwd = opt(args, "cwd").unwrap_or_else(|| ".".to_string());
    let body = opt(args, "body").unwrap_or_default();
    if body.trim().is_empty() {
        return Err("add 需要 --body（给 agent 的首包指令）".into());
    }
    let title = opt(args, "title").unwrap_or_else(|| title_from_prompt(&body));
    let mut task = Task::new(cwd, title, body);

    for (i, a) in args.iter().enumerate() {
        if a.as_str() == "--dep"
            && let Some(dep) = args.get(i + 1)
        {
            task.depends_on.push(dep.clone());
        }
    }

    match opt(args, "auto-run").as_deref() {
        Some("off") => task.auto_run = false,
        _ => {}
    }

    let max = opt(args, "retry-max").and_then(|s| s.parse().ok());
    let delay = opt(args, "retry-delay").and_then(|s| s.parse().ok());
    if max.is_some() || delay.is_some() {
        task.retry_policy = smelt_core::task::TaskRetryPolicy {
            max_attempts: max.unwrap_or(1),
            retry_delay_secs: delay.unwrap_or(0),
            remix_on_retry: false,
        };
    }

    if let Some(schedule) = opt(args, "schedule") {
        if let Some(at) = parse_local_datetime(&schedule) {
            task.kind = TaskKind::Scheduled;
            task.run_at = Some(at);
            task.auto_run = true;
        } else {
            return Err(format!("schedule 格式无法解析：{schedule}（期望 YYYY-MM-DD HH:MM）"));
        }
    }

    let id = task.id.clone();
    request(sock, serde_json::json!({ "op": "task_add", "task": task }))?;
    println!("{id}");
    Ok(())
}

fn cmd_list(args: &[String], sock: &str) -> Result<(), String> {
    let cwd = opt(args, "cwd");
    let all = has_flag(args, "all") || cwd.is_none();
    let as_json = has_flag(args, "json");
    let resp = request(sock, serde_json::json!({ "op": "task_list" }))?;
    let file: smelt_core::task::TaskFile = serde_json::from_value(resp["file"].clone())
        .map_err(|e| format!("smeltd 返回的任务数据非法：{e}"))?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&file).unwrap());
        return Ok(());
    }
    let mut tasks: Vec<_> = file
        .tasks
        .iter()
        .filter(|t| all || t.project_cwd.trim_end_matches('/') == cwd.as_deref().unwrap_or("").trim_end_matches('/'))
        .collect();
    tasks.sort_by_key(|t| t.created_at);
    if tasks.is_empty() {
        println!("（没有任务）");
        return Ok(());
    }
    for t in tasks {
        let col = t.column.label();
        let proj = t
            .project_cwd
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&t.project_cwd);
        let tries = file.runs.iter().filter(|r| r.task_id == t.id).count();
        println!("{:<36}  {:<8}  {:<16}  ×{tries}  {}", t.id, col, proj, t.title);
    }
    Ok(())
}

fn cmd_done(args: &[String], sock: &str) -> Result<(), String> {
    let id = args.first().ok_or("done 需要 <id>")?;
    request(sock, serde_json::json!({ "op": "task_done", "id": id }))?;
    println!("已完成：{id}");
    Ok(())
}

fn cmd_remove(args: &[String], sock: &str) -> Result<(), String> {
    let id = args.first().ok_or("remove 需要 <id>")?;
    request(sock, serde_json::json!({ "op": "task_remove", "id": id }))?;
    println!("已删除：{id}");
    Ok(())
}

fn cmd_show(args: &[String], sock: &str) -> Result<(), String> {
    let id = args.first().ok_or("show 需要 <id>")?;
    let resp = request(sock, serde_json::json!({ "op": "task_list" }))?;
    let file: smelt_core::task::TaskFile = serde_json::from_value(resp["file"].clone())
        .map_err(|e| format!("smeltd 返回的任务数据非法：{e}"))?;
    let Some(task) = file.tasks.iter().find(|t| t.id == *id) else {
        return Err(format!("任务不存在：{id}"));
    };
    let runs = request(sock, serde_json::json!({ "op": "task_runs_for", "task_id": id }))?;
    println!("# {}（{}）", task.title, task.column.label());
    println!("  cwd:   {}", task.project_cwd);
    println!("  body:  {}", task_prompt(task));
    if !task.depends_on.is_empty() {
        println!("  depends_on: {}", task.depends_on.join(", "));
    }
    println!("  runs:");
    if let Some(list) = runs["runs"].as_array() {
        for r in list {
            let status = r["status"].as_str().unwrap_or("?");
            let err = r["error"].as_str().unwrap_or("");
            let err = if err.is_empty() { String::new() } else { format!("  {err}") };
            println!("    #{} {status}  {}{err}", r["attempt"].as_u64().unwrap_or(0), r["launch"].as_str().unwrap_or(""));
        }
    }
    Ok(())
}

fn cmd_run(args: &[String], sock: &str) -> Result<(), String> {
    let id = args.first().ok_or("run 需要 <id>")?;
    let cwd = opt(args, "cwd").unwrap_or_else(|| ".".to_string());
    // run 由 GUI 的 tick/边沿驱动；这里仅触发一次 claim（供 hook/launcher 用）。
    let resp = request(sock, serde_json::json!({ "op": "task_claim", "cwd": cwd }))?;
    if resp["task"].is_null() {
        println!("没有可自动执行的任务");
        Ok(())
    } else {
        let claimed = resp["task"]["id"].as_str().unwrap_or(id);
        println!("已认领：{claimed}");
        Ok(())
    }
}
