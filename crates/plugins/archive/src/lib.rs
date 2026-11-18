//! 归档：各机器上已归档的对话，按机器、项目分组，恢复或彻底删除。
//!
//! 一个视图：侧栏底下「设置」旁边的归档按钮，哪台机器上有归档才露出来，点开是一张弹出
//! 卡片列出全部归档。不占启动栏的入口，也不开整页；布局把它摆在底下那条里。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。归档是运行时工作区目录里对话的一个
//! 状态（`archived_at`），权威在运行时；这里只从宿主转来的目录现算分组，不另存一份。恢复、
//! 删除发运行时的单条目录命令，改完运行时推整份目录，列表随推送变。恢复成功后用会话插件
//! 的 `smelt.sessions/activate` 跳过去。
//!
//! 什么时候重算全靠事件：目录或会话变了（[`HostEvent::SessionsChanged`]）、机器变了、换了
//! 语言。按钮露不露要看有没有归档，所以一直跟着算，不等卡片打开。

use std::collections::HashSet;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use api::model::control_api::{WorkspaceConversationArchive, WorkspaceConversationDelete};
use api::model::workspace_directory::{
    WorkspaceConversationArchiveParams, WorkspaceConversationIdParams,
};
use api::serde_json;
use gpui::*;
use rust_i18n::t;
use smelt_plugin_api::{
    self as api, HostEvent, PluginView, RuntimeKey, ViewHandle, ViewSpec, text,
};

mod model;
mod render;
mod sessions;

use model::{ArchivedRow, ClosedWorkset, MachineGroup};

rust_i18n::i18n!("locales", fallback = "en");

const OWNER: &str = "smelt.archive";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    api::register_views(
        OWNER,
        vec![ViewSpec {
            name: "button".into(),
            title: text!("view.title"),
            icon: api::ui::IconName::Archive.path(),
            // 侧栏底下那条里的一个按钮，不要外壳的标题带。
            draws_top_bar: true,
            instances: false,
            settings: None,
            fills: Vec::new(),
            build: Rc::new(|_, cx| Rc::new(cx.new(Archive::new)) as Rc<dyn ViewHandle>),
        }],
        cx,
    )
    .expect("归档的视图表写错了");
}

pub struct Archive {
    /// 从各机器的工作区目录现算的分组，render 只读它。没有归档的机器不在里面；一台都没有
    /// 时按钮不露。
    machines: Vec<MachineGroup>,
    /// 正在恢复、删除的（机器，对话 id）：按钮先禁用。成了推送会把它从列表里拿走。
    busy: HashSet<(RuntimeKey, String)>,
    /// 选工作树的对话框里选中的那个。
    picked_workset: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl Archive {
    fn new(cx: &mut Context<Self>) -> Self {
        let events = api::events(cx);
        let subscriptions = vec![cx.subscribe(&events, |this, _, event: &HostEvent, cx| {
            // 行上的标题、时间是按当时的语言算好的。
            if matches!(
                event,
                HostEvent::SessionsChanged | HostEvent::MachinesChanged | HostEvent::LocaleChanged
            ) {
                this.rebuild(cx);
            }
        })];
        let mut archive = Self {
            machines: Vec::new(),
            busy: HashSet::new(),
            picked_workset: None,
            _subscriptions: subscriptions,
        };
        archive.rebuild(cx);
        archive
    }

    /// 从各机器的工作区目录重算分组。目录是整份克隆，只在事件里做，不在 render 里。
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let host = api::host(cx);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let machines: Vec<MachineGroup> = host
            .machines(cx)
            .into_iter()
            .filter_map(|machine| {
                let directory = host.workspace_directory(&machine.key, cx)?;
                let style = host.path_style(&machine.key, cx);
                model::machine_group(
                    machine.key,
                    machine.name,
                    machine.unavailable,
                    style,
                    &directory,
                    now,
                )
            })
            .collect();
        if machines != self.machines {
            self.machines = machines;
            cx.notify();
        }
    }

    fn is_busy(&self, machine: &RuntimeKey, id: &str) -> bool {
        self.busy.contains(&(machine.clone(), id.to_string()))
    }

    /// 点了恢复：原工作树已关的先选工作树，别的直接恢复。
    fn request_restore(
        &mut self,
        machine: &RuntimeKey,
        row: &ArchivedRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &row.closed_workset {
            Some(closed) => self.pick_workset(machine, row, closed, window, cx),
            None => self.restore(machine, row, None, cx),
        }
    }

    /// 原工作树已经关了：从同一项目还在的工作树里选一个，对话的工作目录改到那里。
    fn pick_workset(
        &mut self,
        machine: &RuntimeKey,
        row: &ArchivedRow,
        closed: &ClosedWorkset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picked_workset = closed.candidates.first().map(|(id, _)| id.clone());
        let archive = cx.entity();
        let confirm_archive = archive.clone();
        let close_archive = archive.clone();
        let machine = machine.clone();
        let row = row.clone();
        let candidates = closed.candidates.clone();
        api::ui::dialog::Confirm::new(
            t!("restore.closed_title"),
            t!("action.restore"),
            move |_, cx| {
                let Some(workset) = confirm_archive.read(cx).picked_workset.clone() else {
                    return false;
                };
                confirm_archive.update(cx, |archive, cx| {
                    archive.restore(&machine, &row, Some(workset), cx)
                });
                true
            },
        )
        .description(t!("restore.closed_description", root = closed.old_root))
        .body(move |_, cx| render::workset_choices(&archive, &candidates, cx))
        .on_close(move |_, cx| {
            close_archive.update(cx, |archive, _| archive.picked_workset = None);
        })
        .open(window, cx);
    }

    /// 发恢复；成了就跳到那段会话。
    fn restore(
        &mut self,
        machine: &RuntimeKey,
        row: &ArchivedRow,
        target_workset_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let key = (machine.clone(), row.id.clone());
        if !self.busy.insert(key.clone()) {
            return;
        }
        cx.notify();
        let params = WorkspaceConversationArchiveParams {
            conversation_id: row.id.clone(),
            archived: false,
            target_workset_id,
        };
        let call =
            api::call::<WorkspaceConversationArchive>(machine, params, api::CALL_TIMEOUT, cx);
        let machine = machine.clone();
        let session_id = row.session_id.clone();
        let title = row.title.clone();
        cx.spawn(async move |this, cx| {
            let result = call.await;
            this.update(cx, |this, cx| {
                this.busy.remove(&key);
                cx.notify();
                match result {
                    // 终端还没起过的没有会话 id：恢复了就留在会话列表里，不跳。
                    Ok(revision) => {
                        if let Some(session_id) = session_id {
                            let activate = sessions::Activate {
                                machine,
                                session_id,
                                after: Some(revision),
                            };
                            report_failure(sessions::ACTIVATE.run(&activate, cx), cx);
                        }
                    }
                    Err(error) => {
                        notify_error(&t!("restore.failed", title = title, error = error), cx)
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// 彻底删除先确认：归档记录和续接身份一起没了。
    fn confirm_delete(
        &mut self,
        machine: &RuntimeKey,
        row: &ArchivedRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let archive = cx.entity();
        let machine = machine.clone();
        let target = row.clone();
        api::ui::dialog::Confirm::new(t!("delete.confirm"), t!("action.delete"), move |_, cx| {
            archive.update(cx, |archive, cx| archive.delete(&machine, &target, cx));
            true
        })
        .danger()
        .description(t!("delete.description", title = row.title))
        .open(window, cx);
    }

    fn delete(&mut self, machine: &RuntimeKey, row: &ArchivedRow, cx: &mut Context<Self>) {
        let key = (machine.clone(), row.id.clone());
        if !self.busy.insert(key.clone()) {
            return;
        }
        cx.notify();
        let params = WorkspaceConversationIdParams {
            conversation_id: row.id.clone(),
        };
        let call = api::call::<WorkspaceConversationDelete>(machine, params, api::CALL_TIMEOUT, cx);
        let title = row.title.clone();
        cx.spawn(async move |this, cx| {
            let result = call.await;
            this.update(cx, |this, cx| {
                this.busy.remove(&key);
                cx.notify();
                if let Err(error) = result {
                    notify_error(&t!("delete.failed", title = title, error = error), cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

impl PluginView for Archive {
    fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

/// 等别的插件的命令跑完，失败了报出来（命令自己不报）。
fn report_failure(ran: Task<Result<(), String>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        if let Err(error) = ran.await {
            cx.update(|cx| notify_error(&error, cx));
        }
    })
    .detach();
}

fn notify_error(message: &str, cx: &mut App) {
    eprintln!("[archive] {message}");
    let notify = api::Notify {
        subtitle: t!("notify.error").to_string(),
        body: message.to_string(),
        ..Default::default()
    };
    api::NOTIFY.run(&notify, cx).detach();
}
