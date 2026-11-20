// 小人：会话行首会动的 3D 小人头像。
//
// 填会话列表定的插槽 SESSION_AVATAR（约定照抄在 contract.js），会话列表不认识这个插件，删掉它行首
// 回到图标。插槽只给是哪段会话；会话在干什么、露没露着由这里看宿主的目录、会话事实和眼下在看什么，
// 演哪一出见 stage.js。小人长什么样、怎么动由 SmeltBuddy 画：按会话 id 落，每段会话一个，不让人挑。
//
// 状态在模块里一份，每份头像只读它；变了叫所有头像重画。演的过程小人自己按帧画，不用重画这里。

import { View, div } from "gpui-kit";
import { DockArea } from "gpui-base";
import * as api from "smelt-plugin-api";
import { SmeltBuddy } from "smelt-ui";

import { AVATAR_NAME, SESSION_AVATAR } from "./contract.js";
import { conversation_key, empty_stage, greet, track } from "./stage.js";

const stage = empty_stage();
/** 各段会话眼下的状态（键 → 状态），比出变了没有。 */
let seats = new Map();
/** 收到过目录的机器（machine ?? ""）。之后它的目录里新冒出来的会话是刚建的。 */
let known = new Set();

/** 这段对话里的守护会话 id：对话的那个，再是每一格终端（还没起过、没有 id 的不算）。 */
function session_ids(conversation) {
  const ids = [];
  if (conversation.acp) ids.push(conversation.acp.sid);
  for (const terminal of conversation.terminals ?? []) {
    if (terminal.id != null) ids.push(terminal.id);
  }
  return ids;
}

/** 按宿主眼下的目录、会话状态和露着的会话重算。没变就不重画。 */
function sync() {
  const viewed_session = api.active_context().session;
  const next = new Map();
  let viewed = null;
  const fresh = [];
  const machines = new Set();
  for (const { machine } of api.machines()) {
    const directory = api.workspace_directory(machine);
    if (!directory) continue;
    const seen = known.has(machine ?? "");
    for (const fact of api.conversation_facts(machine)) {
      const key = conversation_key(machine, fact.id);
      if (seen && !seats.has(key) && !stage.actors.has(key)) fresh.push(key);
      next.set(key, fact.status);
    }
    if (viewed_session && viewed_session.machine === machine) {
      const conversation = directory.conversations.find((conversation) =>
        session_ids(conversation).includes(viewed_session.id),
      );
      if (conversation) viewed = conversation_key(machine, conversation.id);
    }
    machines.add(machine ?? "");
  }
  known = machines;
  if (same_seats(next, seats) && viewed === stage.selected) return;
  const now = Date.now();
  track(stage, next, viewed, now);
  for (const key of fresh) greet(stage, key, now);
  seats = next;
  api.notify_view();
}

function same_seats(a, b) {
  if (a.size !== b.size) return false;
  for (const [key, status] of a) if (b.get(key) !== status) return false;
  return true;
}

/** 会话行首的头像，每段会话一份。参数第一次打开时定下，之后不换会话。 */
class Avatar extends View {
  init() {
    /** 会话列表给的参数（见 contract.js），写错的当没有。 */
    const args = /** @type {{ machine?: string | null, conversation?: unknown } | null} */ (api.opened_view().args);
    this.conversation = typeof args?.conversation === "string" ? args.conversation : null;
    this.key = this.conversation == null ? null : conversation_key(args.machine, this.conversation);
    if (this.key == null) console.error(`[buddies] 头像的参数不对：${JSON.stringify(args)}`);
  }

  render() {
    if (this.key == null) return div();
    // 没见过的会话（状态还没推到）站着，从 0 起演：种子只看是谁。
    const actor = stage.actors.get(this.key);
    return SmeltBuddy.new(this.conversation, this.conversation, actor?.act ?? "rest", actor?.since ?? 0);
  }
}

async function follow_events() {
  for (;;) {
    const event = await api.next_event();
    try {
      if (
        event.type === "sessions_changed" ||
        event.type === "machines_changed" ||
        event.type === "active_context_changed"
      ) {
        sync();
      }
    } catch (error) {
      console.error(`[buddies] 处理宿主事件 ${event.type} 失败：`, error);
    }
  }
}

export default class Root extends View {
  init() {
    DockArea.register_panel(AVATAR_NAME, Avatar);
    api.register_views([
      {
        name: AVATAR_NAME,
        title: api.text("avatar"),
        icon: "icons/sparkles.svg",
        instances: true,
        fills: [SESSION_AVATAR],
      },
    ]);
    // 宿主可能在本插件装上之前就收到了目录：先按眼下的走一遍，这一遍看到的会话都只按状态摆。
    sync();
    follow_events();
  }

  render() {
    return div();
  }
}
