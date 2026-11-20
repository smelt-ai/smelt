import { expect, test } from "bun:test";

import * as actions from "../actions.js";
import { fake_host, fake_store, list as new_list, local_directory } from "./fakes.mjs";

/** 起好、算过一遍侧栏的列表。 */
function list(host = fake_host(), store = fake_store()) {
  const sessions = new_list(host, store);
  sessions.on_sessions_changed();
  return sessions;
}

/** 记下要确认的，不点。 */
function asker() {
  const asked = [];
  const ask = (request) => asked.push(request);
  return { asked, ask };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

test("置顶项目挪到置顶区最后，存整份顺序", () => {
  const sessions = list();
  expect(sessions.sidebar.project_keys().map((key) => key.root)).toStrictEqual(["/repo", "/tmp/x"]);
  actions.pin_project(sessions, null, "/tmp/x", true);
  expect(sessions.sidebar.project_keys().map((key) => key.root)).toStrictEqual(["/tmp/x", "/repo"]);
  expect(sessions.ui.project_order).toStrictEqual([
    { machine: null, root: "/tmp/x" },
    { machine: null, root: "/repo" },
  ]);
  // 取消置顶：排在其余的最前，不跳回原来的位置。
  actions.pin_project(sessions, null, "/tmp/x", false);
  expect(sessions.sidebar.project_keys().map((key) => key.root)).toStrictEqual(["/tmp/x", "/repo"]);
});

test("拖项目跨过置顶区就跟着置顶", () => {
  const sessions = list();
  actions.pin_project(sessions, null, "/repo", true);
  actions.drop_project(sessions, { machine: null, root: "/tmp/x" }, { machine: null, root: "/repo" }, true);
  const projects = sessions.sidebar.projects;
  expect(projects.map((project) => [project.root, project.pinned])).toStrictEqual([
    ["/tmp/x", true],
    ["/repo", true],
  ]);
  // 落回原处不算挪。
  const writes = sessions.store.writes.length;
  actions.drop_project(sessions, { machine: null, root: "/tmp/x" }, { machine: null, root: "/repo" }, true);
  expect(sessions.store.writes.length).toBe(writes);
});

test("会话只在同一组里换位置，整台机器的顺序跟着改", () => {
  const host = fake_host();
  const { directory, facts } = local_directory();
  directory.conversations.push({ id: "q", terminals: [{ id: "q-t", cwd: "/repo" }], created_at: 1 });
  facts.push({
    id: "q",
    name: "q",
    status: "idle",
    agent: null,
    automation: false,
    cwd: null,
    placement: { kind: "project", root: "/repo", workset: null },
  });
  host.data[""] = { directory, facts, states: [] };
  const sessions = list(host);
  const group = { kind: "project", machine: null, root: "/repo", workset: null };
  expect(sessions.sidebar.group_rows(group).map((row) => row.key.id)).toStrictEqual(["p", "q"]);
  // 整台机器的顺序里只是 p、q 换位，别的会话（工作树里的、别的项目的）不动。
  const swapped = sessions.sidebar.conversation_ids(null).map((id) => (id === "p" ? "q" : id === "q" ? "p" : id));
  actions.drop_session(sessions, { machine: null, id: "q" }, { machine: null, id: "p" }, group, true);
  expect(sessions.sidebar.group_rows(group).map((row) => row.key.id)).toStrictEqual(["q", "p"]);
  expect(sessions.ui.machines.get("").conversation_order).toStrictEqual(swapped);
});

test("闲着的会话直接关，在跑的先问", async () => {
  const host = fake_host();
  const sessions = list(host);
  const { asked, ask } = asker();
  actions.close_session(sessions, ask, { machine: null, id: "p" }, "p", "idle");
  expect(asked.length).toBe(0);
  expect(host.calls.at(-1)).toStrictEqual([null, "workspace.conversation.delete", { conversation_id: "p" }]);
  actions.close_session(sessions, ask, { machine: null, id: "l" }, "l", "running");
  expect(asked.length).toBe(1);
  expect(asked[0].danger).toBe(true);
  expect(host.calls.length).toBe(1);
  asked[0].confirm();
  expect(host.calls.at(-1)).toStrictEqual([null, "workspace.conversation.delete", { conversation_id: "l" }]);
  // 归档不是删，确认按钮不标危险。
  actions.archive_session(sessions, ask, { machine: null, id: "l" }, "l", "needs_you");
  expect(asked[1].danger).toBe(false);
});

test("关不掉的会话把失败挂在那一行上", async () => {
  const host = fake_host();
  host.fail = () => true;
  const sessions = list(host);
  actions.close_session(sessions, () => {}, { machine: null, id: "p" }, "p", "idle");
  await flush();
  const failure = sessions.failures.get("session:@local:p");
  expect(failure.what).toBe("close the session");
  expect(failure.error).toMatch(/运行时拒绝了/);
});

test("项目里有会话才问", () => {
  const sessions = list();
  const { asked, ask } = asker();
  actions.close_project(sessions, ask, null, "/repo", "repo", 0);
  expect(asked.length).toBe(0);
  actions.close_project(sessions, ask, null, "/repo", "repo", 2);
  expect(asked[0].description).toBe("The 2 sessions inside will be closed too.");
});

function inspected(repositories) {
  return { repositories, refresh_failures: [] };
}

test("关工作树：先查，按合并情况问或直接拦下", async () => {
  const host = fake_host();
  const answer = { value: null };
  host.call = async (machine, method, params) => {
    host.calls.push([machine, method, params]);
    if (method === "workspace.workset.inspect") return answer.value;
    if (method === "workspace.workset.finish") return { retained_branches: ["fix"], leftover_entries: [] };
    return null;
  };
  const sessions = list(host);
  const { asked, ask } = asker();

  answer.value = inspected([{ name: "app", dirty: false, merge: "merged" }]);
  await actions.finish_workset(sessions, ask, null, "ws", "fix");
  expect(asked.length).toBe(1);
  expect(asked[0].description).toMatch(/app/);
  asked[0].confirm();
  await flush();
  expect(host.calls.at(-1)[2]).toStrictEqual({ workset_id: "ws", allow_unconfirmed_merge: false });
  // 留下了分支：发通知说一声。
  expect(host.runs.at(-1)[0]).toBe("smelt.app/notify");

  answer.value = inspected([{ name: "app", dirty: false, merge: "unconfirmed" }]);
  await actions.finish_workset(sessions, ask, null, "ws", "fix");
  expect(asked.length).toBe(2);
  asked[1].confirm();
  await flush();
  expect(host.calls.at(-1)[2]).toStrictEqual({ workset_id: "ws", allow_unconfirmed_merge: true });

  answer.value = inspected([{ name: "app", dirty: true, merge: "merged" }]);
  await actions.finish_workset(sessions, ask, null, "ws", "fix");
  expect(asked.length).toBe(2);
  expect(sessions.failures.get("workset:@local:ws").what).toBe("close the worktree");
});

test("拖进来的文件取它所在的目录，一个能用的都没有就报", async () => {
  const host = fake_host();
  const sessions = list(host);
  await actions.open_dropped_paths(sessions, ["/dir/a", "/file/b/c.txt", "/dir/a", "/nope"]);
  await flush();
  const opened = host.calls
    .filter(([, method]) => method === "workspace.project.open")
    .map(([, , params]) => params.root);
  expect(opened).toStrictEqual(["/dir/a", "/file/b"]);

  await actions.open_dropped_paths(sessions, ["/nope"]);
  expect(host.runs.at(-1)[0]).toBe("smelt.app/notify");
});
