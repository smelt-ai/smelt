// state、actions 的测试共用的假宿主、假库和一份本机目录。

import { new_sidebar_ui } from "../model.js";
import { SessionList } from "../state.js";
import { path, t } from "./helpers.mjs";

/** 假宿主：一台本机，目录、派生事实、会话状态都按 machines 给；记下调用和重画。 */
export function fake_host(machines = { "": local_directory() }) {
  const calls = [];
  const runs = [];
  const host = {
    calls,
    runs,
    redraws: 0,
    data: machines,
    machines: () =>
      Object.keys(host.data).map((key) => ({
        machine: key === "" ? null : key,
        name: key || "本机",
        unavailable: null,
      })),
    connections: () => ({ remotes: [] }),
    local_os: "darwin",
    path_style: () => "posix",
    workspace_directory: (machine) => host.data[machine ?? ""]?.directory ?? null,
    session_states: (machine) => host.data[machine ?? ""]?.states ?? [],
    conversation_facts: (machine) => host.data[machine ?? ""]?.facts ?? [],
    session_attention: (machine) => host.data[machine ?? ""]?.attention ?? null,
    path,
    /** 方法 → 回什么（值或 (params) => 值）；没写的回 null。 */
    replies: {},
    async call(machine, method, params) {
      calls.push([machine, method, params]);
      if (host.fail?.(method, params)) throw new Error("运行时拒绝了");
      if (method === "workspace.project.open") return { epoch: "e", revision: 9 };
      const reply = host.replies[method];
      return typeof reply === "function" ? reply(params) : (reply ?? null);
    },
    async run(command, args) {
      runs.push([command, args]);
      if (command === "smelt.app/open-view") return true;
      return host.answers?.[command] ?? null;
    },
    t,
    redraw: () => (host.redraws += 1),
    path_kind: async (path) => (path.startsWith("/dir") ? "dir" : path.startsWith("/file") ? "file" : null),
    parent_dir: (path) => path.slice(0, path.lastIndexOf("/")) || "/",
    now: () => new Date(2026, 10, 20, 12),
    active_context: () => ({ project: host.active ?? null }),
    uuid: () => `id-${(host.uuids = (host.uuids ?? 0) + 1)}`,
    conversation_acp: (settings, option, sid, cwd) => (settings.options.includes(option) ? { option, sid, cwd } : null),
    input: (placeholder) => fake_input(placeholder),
  };
  return host;
}

/** 假输入框：同 SmeltInputState 的 value / set_value / on / release，emit 模拟一次事件。 */
export function fake_input(placeholder) {
  let value = "";
  const handlers = new Map();
  return {
    placeholder,
    released: false,
    value: () => value,
    set_value(next) {
      value = next;
    },
    on(name, handler) {
      handlers.set(name, handler);
    },
    emit(name) {
      handlers.get(name)?.();
    },
    type(next) {
      value = next;
      handlers.get("change")?.();
    },
    release() {
      this.released = true;
    },
  };
}

export function fake_store() {
  const writes = [];
  const record =
    (name) =>
    (...args) =>
      writes.push([name, ...args]);
  return {
    writes,
    set_pinned: record("set_pinned"),
    set_collapsed: record("set_collapsed"),
    set_project_order: record("set_project_order"),
    set_conversation_order: record("set_conversation_order"),
    save_avatar_choice: record("save_avatar_choice"),
    prune: record("prune"),
    forget_machine: record("forget_machine"),
  };
}

export const project = (root, workset = null) => ({ kind: "project", root, workset });
export const fact = (id, placement) => ({
  id,
  name: id,
  status: "idle",
  agent: null,
  automation: false,
  cwd: null,
  placement,
});

export function terminals(id, cwd) {
  return { id, terminals: [{ id: `${id}-t`, cwd }], created_at: 1 };
}

export function local_directory() {
  const directory = {
    epoch: "e",
    revision: 1,
    projects: ["/repo"],
    repositories: [],
    worksets: [{ id: "ws", project_root: "/repo", task: "fix", repositories: [] }],
    conversations: [
      { ...terminals("w", "/elsewhere"), workset_id: "ws" },
      terminals("p", "/repo/sub"),
      terminals("l", "/tmp/x"),
      { ...terminals("old", "/repo"), archived_at: 1 },
    ],
  };
  const facts = [
    fact("w", project("/repo", "ws")),
    fact("p", project("/repo")),
    fact("l", project("/tmp/x")),
    fact("old", { kind: "hidden" }),
  ];
  return { directory, facts, states: [] };
}

export function list(host = fake_host(), store = fake_store()) {
  return new SessionList(host, store, new_sidebar_ui(), { kind: "unset" });
}
