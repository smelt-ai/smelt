//! 两个跨视图通信用的 GPUI 全局单例：ACP 视图（`smelt-acp-view`）和主 GUI 都要
//! 读写，放共享层而不是随便哪一边，免得循环依赖。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gpui::Global;

pub use smelt_core::attention::{AttentionItem, AttentionKind, AttentionStore};
use smelt_core::daemon_state::DaemonSessionState;

/// 守护上报的会话状态镜像（全局单例，跨窗口共享）。key = smeltd session id
/// （每个 pane 一个）/ ACP 会话的 `acp-` 前缀 sid。由主 GUI 启动时那条常驻
/// subscribe 转发任务维护；ACP 视图把自己的相位翻译成这个结构写进来，跟终端
/// 会话共用同一套「四档着色 / Dock 角标 / 应用内通知」链路。
#[derive(Clone, Default)]
pub struct DaemonStates(pub Arc<Mutex<HashMap<String, DaemonSessionState>>>);

impl Global for DaemonStates {}

/// 所有 agent 关注事件的唯一 UI store。生产者写入未读/待投递，Workspace render
/// 统一决定 toast 或系统通知；铃铛、Dock 与菜单栏只读未读集合。
#[derive(Clone, Default)]
pub struct AttentionGlobal(pub Arc<Mutex<AttentionStore>>);

impl Global for AttentionGlobal {}
