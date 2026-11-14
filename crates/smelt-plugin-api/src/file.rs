//! 打开文件：对话里的文件链接、变更视图里的「打开文件」都走 [`open_file`]，谁来打开由
//! 登记的「文件打开者」定，调的一方不认识它。
//!
//! 打开者是插件自己的一条要带参数的命令（[`crate::CommandSpec::requires_args`]），参数是
//! [`FileLocation::to_args`]。用命令而不是另一种回调，JS 插件也能当打开者（执行照常进它的
//! 命令队列）。只认最后登记的那一个：用户插件可以借此替换内置的；它撤下了，之前的接着用。

use gpui::{App, Global, Task};

use crate::RuntimeKey;

/// 要打开的位置：哪台机器上的哪个文件，可选第几行（从 1 数）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLocation {
    pub machine: RuntimeKey,
    /// 那台机器上的绝对路径。
    pub path: String,
    pub line: Option<usize>,
}

impl FileLocation {
    /// 交给打开者命令的参数：`{"machine": null | "<runtime id>", "path": "…", "line": 42 | null}`。
    pub fn to_args(&self) -> serde_json::Value {
        serde_json::json!({
            "machine": crate::machine_to_json(&self.machine),
            "path": self.path,
            "line": self.line,
        })
    }

    /// [`Self::to_args`] 的反过来；形状不对是 `None`。
    pub fn from_args(args: &serde_json::Value) -> Option<Self> {
        Some(Self {
            machine: crate::machine_from_json(args.get("machine")?)?,
            path: args.get("path")?.as_str()?.to_string(),
            line: match args.get("line") {
                None | Some(serde_json::Value::Null) => None,
                Some(line) => Some(usize::try_from(line.as_u64()?).ok()?),
            },
        })
    }
}

/// 登记过的打开者，后登记的在后面。
#[derive(Default)]
struct FileOpeners(Vec<(String, String)>);

impl Global for FileOpeners {}

/// 登记来源 `owner` 的打开者：它的命令 `<owner>/<name>`。同一来源再登记就换掉原来那条并
/// 排到最后。命令要由来源自己另外登记。
pub fn register_file_opener(owner: &str, name: &str, cx: &mut App) {
    let openers = &mut cx.default_global::<FileOpeners>().0;
    openers.retain(|(entry, _)| entry != owner);
    openers.push((owner.to_string(), format!("{owner}/{name}")));
}

/// 撤下来源 `owner` 的打开者。
pub fn unregister_file_opener(owner: &str, cx: &mut App) {
    if cx.has_global::<FileOpeners>() {
        cx.global_mut::<FileOpeners>()
            .0
            .retain(|(entry, _)| entry != owner);
    }
}

/// 现在用哪条命令打开文件：最后登记、命令也还在的那个。
fn opener(cx: &App) -> Option<String> {
    cx.try_global::<FileOpeners>()?
        .0
        .iter()
        .rev()
        .map(|(_, command)| command)
        .find(|command| crate::command(command, cx).is_some())
        .cloned()
}

/// 打开一个文件位置。排到这一轮之后执行（见 [`crate::run`]）。没有打开者时，
/// 本机的文件交给系统文本编辑器，远程的失败。失败给的是可以直接显示的原因。
pub fn open_file(location: FileLocation, cx: &mut App) -> Task<Result<(), String>> {
    if let Some(command) = opener(cx) {
        let opened = crate::run(&command, location.to_args(), cx);
        return cx.spawn(async move |_| opened.await.map(drop));
    }
    match location.machine {
        RuntimeKey::Local => crate::OPEN_IN_TEXT_EDITOR.run(
            &crate::OpenInTextEditor {
                path: location.path.into(),
            },
            cx,
        ),
        RuntimeKey::Remote(_) => Task::ready(Err("没有能打开远程文件的插件".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui::TestAppContext;

    use super::*;
    use crate::{CommandSpec, Handled, register_commands, unregister_commands};

    fn opener_spec(name: &str) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            title: name.to_string(),
            key: None,
            context: "Workspace".to_string(),
            palette: false,
            requires_args: true,
            handler: Rc::new(|_, _, _| Handled::ok()),
        }
    }

    #[test]
    fn location_round_trips_through_command_args() {
        for location in [
            FileLocation {
                machine: RuntimeKey::Local,
                path: "/repo/src/main.rs".into(),
                line: Some(42),
            },
            FileLocation {
                machine: RuntimeKey::Remote("abc".into()),
                path: "/etc/hosts".into(),
                line: None,
            },
        ] {
            assert_eq!(FileLocation::from_args(&location.to_args()), Some(location));
        }
        assert_eq!(
            FileLocation::from_args(&serde_json::json!({"path": 1})),
            None
        );
    }

    #[gpui::test]
    fn the_last_registered_opener_with_a_live_command_wins(cx: &mut TestAppContext) {
        cx.update(|cx| {
            register_commands("files", "文件", vec![opener_spec("open")], cx).unwrap();
            register_commands("mine", "我的", vec![opener_spec("show")], cx).unwrap();
            assert_eq!(opener(cx), None);

            register_file_opener("files", "open", cx);
            register_file_opener("mine", "show", cx);
            assert_eq!(opener(cx).as_deref(), Some("mine/show"));

            // 命令撤了，打开者就跳过它。
            unregister_commands("mine", cx);
            assert_eq!(opener(cx).as_deref(), Some("files/open"));

            register_commands("mine", "我的", vec![opener_spec("show")], cx).unwrap();
            unregister_file_opener("mine", cx);
            assert_eq!(opener(cx).as_deref(), Some("files/open"));

            // 同一来源再登记排到最后。
            register_file_opener("mine", "show", cx);
            register_file_opener("files", "open", cx);
            assert_eq!(opener(cx).as_deref(), Some("files/open"));
        });
    }
}
