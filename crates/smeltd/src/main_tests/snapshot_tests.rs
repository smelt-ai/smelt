use super::*;
use crate::terminal_snapshot::snapshot_ansi_for_handoff;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Processor;

fn visible_text(term: &Term<VoidListener>) -> String {
    term.renderable_content()
        .display_iter
        .map(|i| i.cell.c)
        .filter(|c| *c != '\0')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 把整个网格（含 history）逐格 dump 成文本行——`\0`（宽字符占位格）画成 `·`，
/// 好让 assert 失败时能一眼看出「哪一列开始错位」。
fn grid_dump(term: &Term<VoidListener>) -> Vec<String> {
    let mut rows = Vec::new();
    let mut line = term.topmost_line();
    let bottom = term.bottommost_line();
    while line <= bottom {
        let mut s = String::new();
        for col in 0..term.columns() {
            let c = term.grid()[line][Column(col)].c;
            s.push(if c == '\0' { '·' } else { c });
        }
        rows.push(s);
        line += 1;
    }
    rows
}

fn full_grid_text<T: alacritty_terminal::event::EventListener>(term: &Term<T>) -> String {
    let mut text = String::new();
    let mut line = term.topmost_line();
    let bottom = term.bottommost_line();
    while line <= bottom {
        for col in 0..term.columns() {
            let c = term.grid()[line][Column(col)].c;
            if c != '\0' {
                text.push(c);
            }
        }
        text.push('\n');
        line += 1;
    }
    text
}

/// 逐格 dump **颜色与属性**——`grid_dump` 只比字符，颜色错了它一无所知（真实的
/// reattach bug 正是「字符都在、前景色被恢复成不可见」，字符级对比全绿）。
/// 只 dump 非空格单元，输出紧凑，assert 失败时能直接看出哪个格子的 fg/bg 变了。
fn attr_dump(term: &Term<VoidListener>) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = term.topmost_line();
    let bottom = term.bottommost_line();
    while line <= bottom {
        for col in 0..term.columns() {
            let cell = &term.grid()[line][Column(col)];
            if cell.c == ' ' || cell.c == '\0' {
                continue; // 空白格的前景色无所谓
            }
            out.push(format!(
                "({},{}) {:?} fg={:?} bg={:?} flags={:?}",
                line.0, col, cell.c, cell.fg, cell.bg, cell.flags
            ));
        }
        line += 1;
    }
    out
}

/// 快照的根本契约：**重放后的网格必须和原网格逐格相同**。
/// 比「快照里含某段文本」强得多——丢格、列错位、行粘连都能抓到。
fn assert_roundtrip(rows: usize, cols: usize, input: &str, what: &str) {
    let size = DaemonTermSize { rows, cols };
    let mut a = Term::new(daemon_term_config(), &size, VoidListener);
    let mut pa: Processor = Processor::new();
    pa.advance(&mut a, input.as_bytes());

    let snap = snapshot_ansi(&a, None);

    let mut b = Term::new(daemon_term_config(), &size, VoidListener);
    let mut pb: Processor = Processor::new();
    pb.advance(&mut b, &snap);

    // 颜色/属性必须也一致——真实 bug 就藏在这里，字符级对比看不见。
    let (want_attr, got_attr) = (attr_dump(&a), attr_dump(&b));
    assert_eq!(
        want_attr,
        got_attr,
        "\n{what}：快照重放后**颜色/属性**错了（字符可能都还在）\n快照字节: {:?}\n",
        String::from_utf8_lossy(&snap)
    );

    let (want, got) = (grid_dump(&a), grid_dump(&b));
    assert_eq!(
        want,
        got,
        "\n{what}：快照重放后网格错位\n原始:\n{}\n重放:\n{}\n快照字节: {:?}",
        want.join("\n"),
        got.join("\n"),
        String::from_utf8_lossy(&snap)
    );
}

/// 行尾放不下宽字符：alacritty 在最后一列填 LEADING_WIDE_CHAR_SPACER，宽字符挪到下一行。
/// 快照 `continue` 跳过这个占位格 → 该行只吐 cols-1 个字符 → 不触发自动折行。
#[test]
fn roundtrip_wide_char_at_line_end() {
    assert_roundtrip(4, 8, "abcdefg中x", "行尾宽字符占位格");
}

/// 类 Claude Code 底部状态栏：整行背景色铺满 + 中文 + 边框字形（重启后错位的就是这片）。
#[test]
fn roundtrip_status_bar_like() {
    assert_roundtrip(
        6,
        40,
        "\x1b[44m current  6%  5:30am │ weekly  48% \x1b[0m\r\n\
         \x1b[2m ctx:18% │ cache:100% │ 检查当前模型 \x1b[0m\r\n> ",
        "状态栏（背景色 + 中文 + 竖线）",
    );
}

/// 满行（写满最后一列）后跟硬换行：pending-wrap 状态处理错就会多吞/多吐一行。
#[test]
fn roundtrip_full_width_row_then_newline() {
    assert_roundtrip(4, 6, "abcdef\r\nxy", "满行 + 硬换行");
}

/// 中文占满整行（每字 2 列，正好铺满）。
#[test]
fn roundtrip_cjk_fills_row() {
    assert_roundtrip(4, 6, "中文字\r\nab", "中文铺满行");
}

/// SGR 2（DIM）——Claude Code 状态栏的灰字大量用它。怀疑对象 #1。
#[test]
fn roundtrip_sgr_dim() {
    assert_roundtrip(3, 20, "\x1b[2mdim gray\x1b[0m ok", "DIM 灰字");
}

/// DIM + 前景色组合（暗绿等）。
#[test]
fn roundtrip_sgr_dim_with_color() {
    assert_roundtrip(3, 20, "\x1b[2;32mdimgreen\x1b[0m ok", "DIM + 绿");
}

/// bright black（90）——另一种常见灰。
#[test]
fn roundtrip_sgr_bright_black() {
    assert_roundtrip(3, 20, "\x1b[90mgray\x1b[0m ok", "bright black 灰");
}

/// 256 色前景（38;5;244 = 中灰）。
#[test]
fn roundtrip_sgr_256color() {
    assert_roundtrip(3, 20, "\x1b[38;5;244mgray\x1b[0m ok", "256 色灰");
}

/// 24-bit 真彩前景。
#[test]
fn roundtrip_sgr_truecolor() {
    assert_roundtrip(3, 20, "\x1b[38;2;136;136;136mgray\x1b[0m ok", "真彩灰");
}

/// 状态栏全家桶：灰边框 + DIM + 绿数字 + 中文，一行内多次切色。
#[test]
fn roundtrip_sgr_status_bar_mix() {
    assert_roundtrip(
        4,
        60,
        "\x1b[2m────\x1b[0m\r\n\
         \x1b[2m ctx:\x1b[0m\x1b[32m18%\x1b[0m \x1b[2m│ cache:\x1b[0m\x1b[32m100%\x1b[0m\r\n\
         \x1b[90m current \x1b[0m\x1b[92m11%\x1b[0m \x1b[2m检查模型\x1b[0m",
        "状态栏多色混排",
    );
}

#[test]
fn snapshot_roundtrip_preserves_visible_text() {
    let size = DaemonTermSize { rows: 5, cols: 20 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, b"\x1b[31mhello\x1b[0m\r\nworld");

    let snap = snapshot_ansi(&term, None);
    assert!(snap.windows(5).any(|w| w == b"hello"));
    assert!(snap.windows(5).any(|w| w == b"world"));

    let mut term2 = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser2: Processor = Processor::new();
    parser2.advance(&mut term2, &snap);
    let text = visible_text(&term2);
    assert!(text.contains("hello"), "got {text:?}");
    assert!(text.contains("world"), "got {text:?}");
}

#[test]
fn snapshot_restores_current_sgr_for_following_live_output() {
    let size = DaemonTermSize { rows: 4, cols: 30 };
    let mut original = Term::new(daemon_term_config(), &size, VoidListener);
    let mut original_parser: Processor = Processor::new();
    original_parser.advance(&mut original, b"plain \x1b[1;4;31mstyled");

    let snapshot = snapshot_ansi(&original, None);
    let mut restored = Term::new(daemon_term_config(), &size, VoidListener);
    let mut restored_parser: Processor = Processor::new();
    restored_parser.advance(&mut restored, &snapshot);

    assert_eq!(
        CellStyle::from_cell(&original.grid().cursor.template),
        CellStyle::from_cell(&restored.grid().cursor.template),
        "snapshot must restore the SGR state expected by subsequent PTY diffs"
    );

    original_parser.advance(&mut original, b" live");
    restored_parser.advance(&mut restored, b" live");
    assert_eq!(attr_dump(&original), attr_dump(&restored));
}

#[test]
fn snapshot_enters_alt_screen_when_active() {
    let size = DaemonTermSize { rows: 4, cols: 10 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, b"\x1b[?1049hTUI");
    let snap = snapshot_ansi(&term, None);
    assert!(snap.windows(8).any(|w| w == b"\x1b[?1049h"));
    // Codux 风格 keyframe：备用屏也画可视区内容
    assert!(
        snap.windows(3).any(|w| w == b"TUI"),
        "TUI keyframe 应含可视区文字, got {}",
        String::from_utf8_lossy(&snap)
    );
    assert!(snap.windows(4).any(|w| w == b"\x1b[2J"), "应清屏");
    // 绝对 SGR：每个样式序列以 ESC[0 开头
    assert!(
        snap.windows(4).any(|w| w == b"\x1b[0m") || snap.windows(4).any(|w| w == b"\x1b[0;"),
        "应有绝对 SGR"
    );
}

/// 启动命令是 Codex 不能改变终端的真实模式：主屏快照必须保留 scrollback。
#[test]
fn snapshot_codex_main_screen_keeps_scrollback_history() {
    let size = DaemonTermSize { rows: 3, cols: 40 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    for i in 0..10 {
        parser.advance(&mut term, format!("codex-main-line-{i:02}\r\n").as_bytes());
    }
    assert!(!term.mode().contains(TermMode::ALT_SCREEN));
    assert!(term.history_size() > 0, "fixture must contain scrollback");

    let snap = snapshot_ansi(
        &term,
        Some("codex --dangerously-bypass-approvals-and-sandbox"),
    );
    let snap_text = String::from_utf8_lossy(&snap);
    assert!(
        snap_text.contains("codex-main-line-00"),
        "Codex 主屏 reattach 快照必须含早期历史: {snap_text}"
    );
    assert!(snap_text.contains("codex-main-line-09"));

    let mut restored = Term::new(daemon_term_config(), &size, VoidListener);
    let mut restore_parser: Processor = Processor::new();
    restore_parser.advance(&mut restored, &snap);
    assert!(
        restored.history_size() > 0,
        "Codex 主屏快照重放后必须恢复真实 scrollback"
    );
    let text = full_grid_text(&restored);
    assert!(
        text.contains("codex-main-line-00"),
        "恢复后缺早期历史: {text:?}"
    );
    assert!(
        text.contains("codex-main-line-09"),
        "恢复后缺最新行: {text:?}"
    );
}

/// 真正处于备用屏的 agent 仍只恢复 viewport，不能把主屏历史伪造成当前界面。
#[test]
fn snapshot_alt_screen_agent_keeps_viewport_only() {
    let size = DaemonTermSize { rows: 3, cols: 40 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    for i in 0..6 {
        parser.advance(&mut term, format!("main-only-line-{i:02}\r\n").as_bytes());
    }
    parser.advance(&mut term, b"\x1b[?1049hcodex-alt-screen");
    assert!(term.mode().contains(TermMode::ALT_SCREEN));

    let snap = snapshot_ansi(&term, Some("codex"));
    let snap_text = String::from_utf8_lossy(&snap);
    assert!(snap_text.contains("codex-alt-screen"));
    assert!(
        !snap_text.contains("main-only-line-"),
        "备用屏快照不应携带主屏 scrollback: {snap_text}"
    );

    let handoff_snap = snapshot_ansi_for_handoff(&term, Some("codex"));
    let handoff_text = String::from_utf8_lossy(&handoff_snap);
    assert!(handoff_text.contains("codex-alt-screen"));
    assert!(
        !handoff_text.contains("main-only-line-"),
        "备用屏 handoff 快照不应携带主屏 scrollback: {handoff_text}"
    );

    let mut restored = Term::new(daemon_term_config(), &size, VoidListener);
    let mut restore_parser: Processor = Processor::new();
    restore_parser.advance(&mut restored, &snap);
    assert!(restored.mode().contains(TermMode::ALT_SCREEN));
    assert_eq!(restored.history_size(), 0, "备用屏不应恢复主屏 scrollback");
}

#[test]
fn watch_snapshot_keeps_main_screen_history_for_agent_launch() {
    let size = DaemonTermSize { rows: 3, cols: 40 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    for i in 0..10 {
        parser.advance(&mut term, format!("agent-line-{i:02}\r\n").as_bytes());
    }

    let snap = snapshot_ansi_for_watch(&term, Some("codex"), SNAPSHOT_MAX_LINES);
    assert!(snap.windows(13).any(|w| w == b"agent-line-00"));
    assert!(snap.windows(13).any(|w| w == b"agent-line-09"));
}

/// 主屏 TUI（不切备用屏、完全靠终端滚动的那类）的 history 能堆到上万行，全推给
/// 手机就是进页面先干等。消费端声明预算后，快照只带最新的那一段。
#[test]
fn watch_snapshot_honours_consumer_scrollback_budget() {
    let size = DaemonTermSize { rows: 3, cols: 40 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    for i in 0..200 {
        parser.advance(&mut term, format!("line-{i:03}\r\n").as_bytes());
    }

    let budgeted = snapshot_ansi_for_watch(&term, None, 20);
    assert!(
        !budgeted.windows(8).any(|w| w == b"line-000"),
        "预算之外的老内容不该出现在快照里"
    );
    assert!(budgeted.windows(8).any(|w| w == b"line-199"));

    let full = snapshot_ansi_for_watch(&term, None, SNAPSHOT_MAX_LINES);
    assert!(full.windows(8).any(|w| w == b"line-000"));
    assert!(
        budgeted.len() * 4 < full.len(),
        "裁剪后省下的必须是字节，不只是行数：budgeted={} full={}",
        budgeted.len(),
        full.len()
    );
}

/// 预算再小也得够铺满可视区，否则光标定位会落在快照之外。
#[test]
fn watch_snapshot_budget_never_drops_below_viewport() {
    let size = DaemonTermSize { rows: 5, cols: 20 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    for i in 0..50 {
        parser.advance(&mut term, format!("row-{i:02}\r\n").as_bytes());
    }

    let snap = snapshot_ansi_for_watch(&term, None, 1);
    // 可视区是最后 5 行：47/48/49 加上末尾空行，至少要能看到 row-47。
    assert!(snap.windows(6).any(|w| w == b"row-47"));
}

/// 备用屏 TUI 本来就只画可视区，可用行数就是屏高——客户端据此知道「上面没有更老的」。
#[test]
fn available_lines_reports_viewport_on_alt_screen() {
    let size = DaemonTermSize { rows: 4, cols: 20 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    for i in 0..100 {
        parser.advance(&mut term, format!("h-{i:02}\r\n").as_bytes());
    }
    assert!(
        available_snapshot_lines(&term) > 50,
        "主屏要报出全部 history"
    );

    parser.advance(&mut term, b"\x1b[?1049h");
    assert_eq!(available_snapshot_lines(&term), 4);
}

/// 真彩 SGR 必须以完整 `\x1b[0;…48;2;…m` 形式出现（Codux 绝对 SGR）。
#[test]
fn snapshot_truecolor_sgr_always_has_esc_prefix() {
    let size = DaemonTermSize { rows: 3, cols: 20 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, b"\x1b[48;2;20;20;20mX\x1b[0m");
    let snap = snapshot_ansi(&term, None);
    let s = String::from_utf8_lossy(&snap);
    // 实际形如 \x1b[0;39;48;2;20;20;20m
    assert!(
        s.contains("\u{1b}[0;39;48;2;20;20;20m")
            || s.contains("\u{1b}[0;") && s.contains("48;2;20;20;20m"),
        "绝对 SGR 应含完整真彩序列: {s}"
    );
    // 重放后字符仍在
    let mut term2 = Term::new(daemon_term_config(), &size, VoidListener);
    let mut p2: Processor = Processor::new();
    p2.advance(&mut term2, &snap);
    assert!(visible_text(&term2).contains('X'));
}

/// 宿主带着 NO_COLOR=1 时，交互式 PTY 仍必须是彩色终端。
#[test]
fn interactive_pty_strips_host_color_suppression() {
    let mut cmd = CommandBuilder::new("/bin/zsh");
    cmd.env("NO_COLOR", "1");
    cmd.env("FORCE_COLOR", "0");
    cmd.env("CLICOLOR", "0");
    cmd.env("CLICOLOR_FORCE", "0");
    cmd.env("TERM", "dumb");
    apply_interactive_pty_env(&mut cmd);

    assert_eq!(
        cmd.get_env("TERM").and_then(|v| v.to_str()),
        Some("xterm-256color")
    );
    assert_eq!(
        cmd.get_env("COLORTERM").and_then(|v| v.to_str()),
        Some("truecolor")
    );
    for key in smelt_core::tty_color::SUPPRESSION_VARS {
        assert!(
            cmd.get_env(key).is_none(),
            "{key} 不得带进交互式 PTY，实际: {:?}",
            cmd.get_env(key)
        );
    }
}

#[test]
fn agent_launch_reads_interactive_shell_config() {
    assert_eq!(
        shell_launch_args("/bin/zsh", Some("claude --dangerously-skip-permissions")),
        vec![
            "-ilc".to_string(),
            "claude --dangerously-skip-permissions; exec /bin/zsh -l".to_string(),
        ]
    );
    assert_eq!(shell_launch_args("/bin/zsh", None), vec!["-l".to_string()]);
}

#[test]
fn mcp_launch_detection_handles_env_prefixes_and_absolute_paths() {
    use smelt_core::agent_kind::ConversationAgentKind;

    assert_eq!(
        launch_agent_kind("CLAUDE_CONFIG_DIR=/tmp/profile claude --resume abc"),
        Some(ConversationAgentKind::Claude)
    );
    assert_eq!(
        launch_agent_kind("env CODEX_HOME=/tmp/codex /opt/homebrew/bin/codex resume abc"),
        Some(ConversationAgentKind::Codex)
    );
    assert_eq!(
        launch_agent_kind("cursor-agent --force"),
        Some(ConversationAgentKind::Cursor)
    );
    assert_eq!(
        launch_agent_kind("opencode --auto"),
        Some(ConversationAgentKind::OpenCode)
    );
    assert_eq!(launch_agent_kind("zsh -l"), None);
}

#[test]
fn snapshot_includes_scrollback_history() {
    // 3 行屏高，灌 10 行 → 前几行进 history
    let size = DaemonTermSize { rows: 3, cols: 40 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    for i in 0..10 {
        parser.advance(&mut term, format!("line-{i:02}\r\n").as_bytes());
    }
    // 可视区只有最后几行；快照必须仍带上更早的 line-00
    let snap = snapshot_ansi(&term, None);
    assert!(
        snap.windows(7).any(|w| w == b"line-00"),
        "完整快照应含 scrollback 里的 line-00，实际: {}",
        String::from_utf8_lossy(&snap)
    );
    assert!(snap.windows(7).any(|w| w == b"line-09"));

    // 重放到同尺寸终端，早期行必须进入真实 scrollback，而不是用越界 CUP
    // 全部夹在可视区底部。
    let mut term2 = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser2: Processor = Processor::new();
    parser2.advance(&mut term2, &snap);
    assert!(
        term2.topmost_line().0 < 0,
        "同尺寸重放后应产生 scrollback，topmost={:?}",
        term2.topmost_line()
    );
    // 扫整个 grid（含 history）
    let mut all = String::new();
    let top = term2.topmost_line();
    let bottom = term2.bottommost_line();
    let mut line = top;
    while line <= bottom {
        for col in 0..term2.columns() {
            all.push(term2.grid()[line][Column(col)].c);
        }
        all.push('\n');
        line += 1;
    }
    assert!(
        all.contains("line-00"),
        "重放后 grid 应含 line-00，got {all:?}"
    );
    assert!(all.contains("line-09"), "重放后 grid 应含 line-09");
}

#[test]
fn snapshot_restores_bracketed_paste_mode() {
    let size = DaemonTermSize { rows: 3, cols: 10 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, b"\x1b[?2004hhi");
    let snap = snapshot_ansi(&term, None);
    assert!(
        snap.windows(8).any(|w| w == b"\x1b[?2004h"),
        "开了 bracketed paste 的会话快照应恢复该模式"
    );
}

#[test]
fn snapshot_preserves_osc8_hyperlink() {
    let size = DaemonTermSize { rows: 3, cols: 40 };
    let mut term = Term::new(daemon_term_config(), &size, VoidListener);
    let mut parser: Processor = Processor::new();
    parser.advance(
        &mut term,
        b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
    );
    let snap = snapshot_ansi(&term, None);
    let s = String::from_utf8_lossy(&snap);
    assert!(
        s.contains("https://example.com"),
        "快照应含 OSC 8 URI，got {s}"
    );
    assert!(snap.windows(4).any(|w| w == b"link"));
}
