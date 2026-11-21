//! 原生 Metal 离屏出图，人工核对真实插件视图。运行：
//! cargo test -j 2 -p smelt-office office_snapshots -- --ignored --nocapture

use std::path::Path;
use std::sync::Arc;

use gpui::{AppContext as _, HeadlessAppContext, size};
use smelt_plugin_api::model::theme_index::{DEFAULT_DARK, DEFAULT_LIGHT};

use super::*;

// ui_theme 的调色板在进程内共享，两种原生导出不能同时切换主题。
pub(super) static EXPORT: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn demo_rooms() -> Vec<Room> {
    use api::model::agent_kind::{ConversationAgentKind, TerminalAgentKind};
    use api::model::daemon_state::{DaemonPhase, DaemonSessionState};
    use api::model::runtime_path::PathStyle;
    use api::model::workspace_directory::{
        WorkspaceAcp, WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
    };

    let mut directory = WorkspaceDirectory {
        projects: vec!["/projects/smelt".into()],
        ..Default::default()
    };
    let mut states = Vec::new();
    let phases = [
        DaemonPhase::ExecutingTool,
        DaemonPhase::Thinking,
        DaemonPhase::AwaitingApproval,
        DaemonPhase::WaitingForUser,
        DaemonPhase::Failed,
        DaemonPhase::Idle,
    ];
    for (index, kind) in ConversationAgentKind::ALL
        .into_iter()
        .chain([ConversationAgentKind::Custom])
        .enumerate()
    {
        let id = format!("c-{}", kind.id());
        directory.conversations.push(WorkspaceConversation {
            id: id.clone(),
            custom_title: Some(kind.label().into()),
            acp: Some(WorkspaceAcp {
                sid: id.clone(),
                agent: kind.id().into(),
                cwd: Some("/projects/smelt".into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        states.push(DaemonSessionState {
            id,
            phase: phases[index % phases.len()],
            turn_events: true,
            ..Default::default()
        });
    }
    for kind in TerminalAgentKind::ALL
        .into_iter()
        .filter(|kind| kind.acp().is_none())
    {
        directory.conversations.push(WorkspaceConversation {
            id: format!("c-{}", kind.id()),
            custom_title: Some(kind.label().into()),
            terminals: vec![WorkspaceTerminal {
                cwd: Some("/projects/smelt".into()),
                launch_cmd: Some(kind.quick_terminal_cmd().into()),
                ..Default::default()
            }],
            ..Default::default()
        });
    }
    let machine = api::Machine {
        key: api::RuntimeKey::Local,
        name: "MacBook Pro".into(),
        unavailable: None,
    };
    let mut rooms = model::rooms_for_machine(&machine, true, PathStyle::Posix, &directory, &states);
    directory.conversations.truncate(1);
    directory.projects = vec!["/work/api".into()];
    directory.conversations[0].acp.as_mut().unwrap().cwd = Some("/work/api".into());
    let remote = api::Machine {
        key: api::RuntimeKey::Remote("server".into()),
        name: "Linux server".into(),
        unavailable: Some("Connection lost".into()),
    };
    rooms.extend(model::rooms_for_machine(
        &remote,
        false,
        PathStyle::Posix,
        &directory,
        &states,
    ));
    rooms
}

#[test]
#[ignore = "native visual verification"]
fn office_snapshots() {
    let _export = EXPORT.lock().unwrap();
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/tmp/snapshots/office");
    std::fs::create_dir_all(&out).unwrap();
    for (theme, suffix) in [(DEFAULT_LIGHT, "light"), (DEFAULT_DARK, "dark")] {
        for (width, height, name) in [(1080., 1200., "wide"), (380., 840., "narrow")] {
            let mut cx = HeadlessAppContext::with_platform(
                Arc::new(gpui_apple::AppleTextSystem::new()),
                Arc::new(api::ui::assets::Assets),
                || {
                    Ok(Some(Box::new(
                        gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
                    )))
                },
            );
            cx.update(|cx| {
                gpui_base::init(cx);
                api::ui::init(cx);
                assert!(api::apply_theme(theme, cx));
            });
            let window = cx
                .open_window(size(px(width), px(height)), |window, cx| {
                    let office = cx.new(|cx| {
                        Office::preview(demo_rooms(), cx.background_executor().now(), None)
                    });
                    cx.new(|cx| gpui_base::Root::new(office, window, cx))
                })
                .unwrap();
            for _ in 0..3 {
                cx.run_until_parked();
                cx.update_window(window.into(), |_, window, cx| {
                    window.draw(cx).clear(cx);
                })
                .unwrap();
            }
            let path = out.join(format!("office-{name}-{suffix}.png"));
            cx.capture_screenshot(window.into())
                .unwrap()
                .save(&path)
                .unwrap();
            println!("{}", path.display());
        }
    }
}

/// 连续帧来自真实的 Office::render + scene::workstation；只有状态输入和时钟是演示数据。
#[test]
#[ignore = "export native animation frames"]
fn office_animation_demo() {
    let _export = EXPORT.lock().unwrap();
    use std::hash::{Hash as _, Hasher as _};
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let out = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/tmp/snapshots/office/animation");
    std::fs::create_dir_all(&out).unwrap();
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(gpui_apple::AppleTextSystem::new()),
        Arc::new(api::ui::assets::Assets),
        || {
            Ok(Some(Box::new(
                gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
            )))
        },
    );
    cx.update(|cx| {
        gpui_base::init(cx);
        api::ui::init(cx);
        assert!(api::apply_theme(DEFAULT_LIGHT, cx));
        api::set_locale("zh-CN", cx);
    });
    let mut rooms = demo_rooms();
    rooms.truncate(1);
    rooms[0] = super::benchmarks::capacity_room(&rooms[0], 100, Activity::Idle);
    rooms[0].name = "100 个对话 · 滚动演示".into();
    for (index, worker) in rooms[0].workers.iter_mut().enumerate() {
        worker.title = format!("{:03} · {}", index + 1, worker.provider);
    }
    let epoch = cx.background_executor.now();
    let office = cx.new(|_| Office::preview(rooms, epoch, Some(epoch)));
    let window = cx
        .open_window(size(px(1000.), px(880.)), |window, cx| {
            cx.new(|cx| gpui_base::Root::new(office.clone(), window, cx))
        })
        .unwrap();
    let video_path = out.parent().unwrap().join("office-demo.mp4");
    let mut encoder = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgba",
            "-video_size",
            "2000x1760",
            "-framerate",
            "12",
            "-i",
            "pipe:0",
            "-vf",
            "scale=1000:880:flags=neighbor",
            "-an",
            "-c:v",
            "libx264",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(&video_path)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ffmpeg is required to export the demo");
    let mut input = encoder.stdin.take().unwrap();
    let mut previous_stage = 0;
    let mut hashes = Vec::new();
    for index in 0..192 {
        let seconds = index as f64 / 12.;
        let stage = if seconds < 2.5 {
            0
        } else if seconds < 4.5 {
            1
        } else if seconds < 6.5 {
            2
        } else if seconds < 9.5 {
            3
        } else if seconds < 12. {
            4
        } else {
            5
        };
        let now = epoch + Duration::from_secs_f64(seconds);
        office.update(&mut cx, |office, cx| {
            if stage != previous_stage {
                for (index, worker) in office.rooms[0].workers.iter_mut().enumerate() {
                    worker.completed = stage == 5;
                    worker.activity = match stage {
                        1 => Activity::Thinking,
                        2 | 4 => Activity::Executing,
                        3 => [
                            Activity::Approval,
                            Activity::Input,
                            Activity::Failed,
                            Activity::Offline,
                        ][index % 4],
                        _ => Activity::Idle,
                    };
                }
                office.cast.sync(&office.rooms, now);
            }
            if seconds > 5. && seconds < 11. {
                office.list.scroll_by(px(70.));
            }
            office.preview_time = Some(now);
            cx.notify();
        });
        previous_stage = stage;
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .unwrap();
        let screenshot = cx.capture_screenshot(window.into()).unwrap();
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        screenshot.as_raw().hash(&mut hash);
        hashes.push(hash.finish());
        assert_eq!((screenshot.width(), screenshot.height()), (2000, 1760));
        input.write_all(screenshot.as_raw()).unwrap();
        if [6, 36, 60, 84, 102, 126, 150, 191].contains(&index) {
            screenshot
                .save(out.join(format!("frame-{index:03}.png")))
                .unwrap();
        }
    }
    drop(input);
    let result = encoder.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_ne!(hashes[0], hashes[6], "入场必须实际移动");
    assert_ne!(hashes[54], hashes[56], "镜头静止时工作帧必须变化");
    assert!(
        cx.update(|cx| office.read(cx).list.logical_scroll_top().item_ix >= 22),
        "录像必须滚到最后几个工位"
    );
    assert_eq!(hashes[190], hashes[191], "休息收尾后必须稳定为静态");
    println!("Native GPUI recording: {}", video_path.display());
}
