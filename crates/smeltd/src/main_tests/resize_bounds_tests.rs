//! 端到端走真实的 `handle_conn` 分发，而不是只测 action_payload 这个纯函数——
//! 门闩逻辑（phase 不对就拒绝、不实际写入）本身也得有测试盯着，不能只信任
//! action_payload 测过就够了。

use super::*;

/// 同 action_integration_tests::make_pipe_session 的构造，独立一份是为了让这
/// 组测试不依赖那边的 Phase/管道语义——这里只关心几何。
fn make_session(rows: u16, cols: u16) -> Arc<Session> {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
    // 读端留着不关，否则往写端 resize 时可能吃 SIGPIPE。
    let read_end = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    std::mem::forget(read_end);
    let master = unsafe { std::fs::File::from_raw_fd(fds[1]) };

    let state = Arc::new(Mutex::new(SessionState::default()));
    let color_replies = Arc::new(Mutex::new(VecDeque::new()));
    let listener = StateListener::with_color_replies(
        Arc::clone(&state),
        new_event_hub(),
        Arc::clone(&color_replies),
    );
    Arc::new(Session {
        instance: next_session_instance(),
        geometry_token: uuid::Uuid::new_v4().simple().to_string(),
        child: TerminalChild::start(-1).unwrap(),
        ctl: Mutex::new(Ctl {
            master,
            jolt: false,
            cols,
            rows,
            cell_w: 0,
            cell_h: 0,
            remote_viewports: 0,
            remote_grace: 0,
            cwd: None,
        }),
        input_gate: Mutex::new(()),
        out: Mutex::new(Out {
            clients: Vec::new(),
            watchers: Vec::new(),
        }),
        output_gate: Mutex::new(()),
        color_replies,
        term: Mutex::new(new_daemon_term(rows, cols, listener)),
        state,
    })
}

/// 网格是一次性分配出来的，所以一个离谱的尺寸不是"显示得难看"，是 abort 掉
/// 守护进程、把这台机器上所有会话一起带走。in-band resize 帧不像 attach 的
/// JSON 那样在调用点做过约束，任何能连上 socket 的东西都能发，所以下界必须
/// 落在它们共同经过的这一层。
#[test]
fn an_absurd_remote_resize_is_clamped_instead_of_killing_the_daemon() {
    let sess = make_session(24, 80);

    resize_session_remote(&sess, u16::MAX, u16::MAX, u16::MAX, u16::MAX);

    let ctl = sess.ctl.lock().unwrap();
    assert_eq!(ctl.cols, MAX_SESSION_COLS);
    assert_eq!(ctl.rows, MAX_SESSION_ROWS);
    assert_eq!(ctl.cell_w, MAX_SESSION_CELL_PX);
    assert_eq!(ctl.cell_h, MAX_SESSION_CELL_PX);
}

/// 钳制只该对离谱值生效——真实终端尺寸必须原样落地，否则这个补丁就把正常
/// 的 resize 一起改坏了。
#[test]
fn an_ordinary_resize_still_lands_untouched() {
    let sess = make_session(24, 80);

    resize_session_remote(&sess, 120, 40, 9, 18);

    let ctl = sess.ctl.lock().unwrap();
    assert_eq!(ctl.cols, 120);
    assert_eq!(ctl.rows, 40);
    assert_eq!(ctl.cell_w, 9);
    assert_eq!(ctl.cell_h, 18);
}

/// 零是"没测到"的意思，不是"要一个 0 宽的终端"。
#[test]
fn a_zero_sized_resize_falls_back_to_one_cell() {
    let sess = make_session(24, 80);

    resize_session_remote(&sess, 0, 0, 0, 0);

    let ctl = sess.ctl.lock().unwrap();
    assert_eq!(ctl.cols, 1);
    assert_eq!(ctl.rows, 1);
}
