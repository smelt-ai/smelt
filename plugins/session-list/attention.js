// 各台机器上哪些守护会话有没看过的提醒（跑完了、失败了、等你批准……你还没看）。会话列表按它
// 把标题提亮、在可视区外时提示。
//
// 权威在运行时（smeltd），这里只照它推来的 attention.changed 留一份只读缓存，不自己判定、不标
// 已读。订上（含重连）时先来整份快照，之后逐条变化，每条带递增的 revision：快照整份换掉，比手
// 里旧的变化（快照前排着的）丢掉。

import { machine_key } from "./model.js";

export class Unread {
  constructor() {
    /** machine_key → { primed, revision, sessions: Set<守护会话 id> } */
    this.machines = new Map();
  }

  /** 收下一条推送（TopicMessage）。有没看过的会话变了是 true。 */
  apply(machine, message) {
    const key = machine_key(machine);
    if (message.kind === "snapshot") {
      const snapshot = readable(machine, message.payload, (value) => Array.isArray(value?.sessions));
      if (!snapshot) return false;
      const sessions = new Set(
        snapshot.sessions.filter((session) => session.unread != null).map((session) => session.session_id),
      );
      const before = this.machines.get(key)?.sessions ?? new Set();
      const changed = !same_set(before, sessions);
      this.machines.set(key, { primed: true, revision: snapshot.revision, sessions });
      return changed;
    }
    if (message.kind === "event") {
      const changed = readable(machine, message.payload, (value) => typeof value?.session?.session_id === "string");
      if (!changed) return false;
      let entry = this.machines.get(key);
      if (!entry) {
        entry = { primed: false, revision: 0, sessions: new Set() };
        this.machines.set(key, entry);
      }
      if (entry.primed && changed.revision <= entry.revision) return false;
      entry.revision = changed.revision;
      const id = changed.session.session_id;
      if (changed.session.unread != null) {
        if (entry.sessions.has(id)) return false;
        entry.sessions.add(id);
        return true;
      }
      return entry.sessions.delete(id);
    }
    // 积压时后面跟着一份快照；这一轮不会再推时留着上次收到的，重新连上时再订。
    return false;
  }

  /** 机器移除了，它的提醒跟着没了。 */
  forget(machine) {
    this.machines.delete(machine_key(machine));
  }

  has(machine, session_id) {
    return this.machines.get(machine_key(machine))?.sessions.has(session_id) ?? false;
  }
}

/** 读不懂的载荷记一笔丢掉。 */
function readable(machine, value, ok) {
  if (ok(value) && typeof value.revision === "number") return value;
  console.error(`[sessions] ${machine ?? "本机"} 的提醒读不懂：${JSON.stringify(value)}`);
  return null;
}

function same_set(a, b) {
  if (a.size !== b.size) return false;
  for (const item of a) if (!b.has(item)) return false;
  return true;
}
