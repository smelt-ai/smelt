// 会话列表插件的根：登记视图、命令、话题，拼好给逻辑层用的宿主（host），跟着宿主的事件流和命令走。
// 根画在零大小的一层里，只放命令打开的对话框（⌘N 的新建会话菜单、项目的仓库对话框）。
//
// 状态都在 SessionList（state.js）上，列表视图、设置页和这里共用。

import { randomUUID } from "crypto";
import { stat } from "fs/promises";
import { platform } from "os";
import { dirname, join } from "path";

import { div, View } from "gpui-kit";
import { DockArea } from "gpui-base";
import * as api from "smelt-plugin-api";
import { SmeltInputState } from "smelt-ui";

import {
  FOCUS_LIST,
  LIST_NAME,
  NEW_SESSION,
  NEXT_SESSION,
  OPEN_PROJECT,
  OWNER,
  PREV_SESSION,
  REVEAL_IN_LIST,
  SETTINGS_NAME,
} from "./contract.js";
import { list_view_class, open_list } from "./list_view.js";
import { new_sidebar_ui } from "./model.js";
import { palette_dialog } from "./new_session_view.js";
import { repositories_dialog } from "./repositories_view.js";
import { settings_view_class } from "./settings_view.js";
import { ATTENTION_TOPIC, SessionList } from "./state.js";
import { ListStore } from "./store.js";
import { message } from "./util.js";

const host = {
  machines: api.machines,
  connections: api.connections,
  local_os: platform(),
  path_style: api.path_style,
  workspace_directory: api.workspace_directory,
  session_states: api.session_states,
  conversation_facts: api.conversation_facts,
  active_context: api.active_context,
  path: {
    clean: api.path_clean,
    file_name: api.path_file_name,
    join: api.path_join,
    is_absolute: api.path_is_absolute,
    starts_with: api.path_starts_with,
  },
  project_labels: api.project_labels,
  call: api.call,
  run: api.run,
  t: api.t,
  redraw: () => api.notify_view(),
  path_kind: async (path) => {
    try {
      const found = await stat(path);
      return found.isDirectory() ? "dir" : found.isFile() ? "file" : null;
    } catch {
      return null;
    }
  },
  parent_dir: dirname,
  now: () => new Date(),
  uuid: randomUUID,
  input: (placeholder) => SmeltInputState.new(placeholder),
  conversation_acp: api.conversation_acp,
};

const store = new ListStore({
  query: api.store_query,
  execute: api.store_execute,
  write: api.store_write,
  migrate: api.store_migrate,
});
const list = new SessionList(host, store, new_sidebar_ui(), { kind: "unset" });

/** 搜索词两种语言都写：用哪种语言搜都找得到。 */
const SETTINGS_KEYWORDS = ["头像", "小人", "行首", "侧栏", "avatar", "buddy", "sidebar", "sessions"];

/** 命令 id 里插件 id 后面那段。 */
const name_of = (id) => id.slice(OWNER.length + 1);

/** 不带参数的命令：进命令面板，可以绑键，在主窗口任何地方生效。 */
const shortcut = (id, title, key) => ({ name: name_of(id), title: api.text(title), key, context: "Workspace", args: [] });

const COMMANDS = [
  {
    name: name_of(REVEAL_IN_LIST),
    title: api.text("command.reveal_in_list"),
    requires_args: true,
    args: ["machine", "session_id"],
  },
  shortcut(NEW_SESSION, "menu.new_session"),
  shortcut(OPEN_PROJECT, "command.open_project"),
  // ⌘⇧E 跟 VS Code「聚焦资源管理器」一样。
  shortcut(FOCUS_LIST, "command.focus_list", "cmd-shift-e"),
  // 绑在 Workspace 上比输入框浅：焦点在输入框里时是文首 / 文末。
  shortcut(PREV_SESSION, "command.prev_session", "cmd-up"),
  shortcut(NEXT_SESSION, "command.next_session", "cmd-down"),
];

/** 执行一条本插件的命令，兑现为交回的结果。 */
function run_command(call) {
  switch (call.name) {
    case name_of(REVEAL_IN_LIST):
      return list.reveal_in_list(call.args);
    case name_of(NEW_SESSION):
      list.open_palette();
      return null;
    case name_of(OPEN_PROJECT):
      return list.pick_projects();
    case name_of(FOCUS_LIST):
      return list.focus_list();
    case name_of(PREV_SESSION):
      return list.step_session(false);
    case name_of(NEXT_SESSION):
      return list.step_session(true);
  }
  throw new Error(`不认识的命令：${call.name}`);
}

async function serve_commands() {
  for (;;) {
    const call = await api.next_command();
    Promise.resolve()
      .then(() => run_command(call))
      .then(
        (result) => api.resolve_command(call.id, result ?? null),
        (error) => api.reject_command(call.id, message(error)),
      );
  }
}

async function follow_events() {
  for (;;) {
    const event = await api.next_event();
    try {
      if (event.type === "view_opened") open_list(event.instance, event.args);
      else list.on_event(event);
    } catch (error) {
      console.error(`[session-list] 处理宿主事件 ${event.type} 失败：`, error);
    }
  }
}

/** 读回库里的侧栏状态和头像选择，再按眼下的目录、选中、智能体配置走一遍。 */
async function start() {
  // 会话插件那份旧库：和本库同在 plugin-data 下。新建本库时从那边抄一次侧栏的表。
  const style = api.path_style(null);
  const own = api.path_clean(style, api.data_dir());
  await store.open(join(dirname(own), "smelt.sessions", "store.sqlite3"));
  list.loaded(await store.read_sidebar(), await store.avatar_choice());
  await list.start();
}

export default class Root extends View {
  init(_props, cx) {
    /** ⌘N 那张开好时交给菜单的焦点。 */
    this.palette_focus = cx.focus_handle();
    DockArea.register_panel(LIST_NAME, list_view_class(list));
    DockArea.register_panel(SETTINGS_NAME, settings_view_class(list));
    api.register_views([
      { name: LIST_NAME, title: api.text("view.list"), icon: "icons/messages-square.svg", draws_top_bar: true },
      {
        name: SETTINGS_NAME,
        title: api.text("settings.title"),
        icon: "icons/messages-square.svg",
        settings: { group: "general", keywords: SETTINGS_KEYWORDS },
      },
    ]);
    api.register_commands(COMMANDS);
    api.register_topics([ATTENTION_TOPIC]);
    follow_events();
    serve_commands();
    start().catch((error) => console.error("[session-list] 起来失败：", error));
  }

  render() {
    return div()
      .when(list.palette != null, (root) => root.child(palette_dialog(list, this.palette_focus)))
      .when(list.repositories != null, (root) => root.child(repositories_dialog(list.repositories)));
  }
}
