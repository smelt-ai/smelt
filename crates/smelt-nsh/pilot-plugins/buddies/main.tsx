// 小人：plugins/buddies 在 NSH 试点里的移植，行为照它的 main.js。演哪一出在 stage.js，原样拷来的。
// 试点里由外壳按 id 摆，所以不登记视图、不填插槽。

import { host, nativeComponent, signal, View } from "nsh";

import { conversation_key, empty_stage, greet, track } from "./stage.js";

/** 一个小人的格子（SmeltBuddy 的 36 pt）。 */
const CELL = 36;

const Buddy = nativeComponent<{ seed: string; act: string; since: number }, {}>("smelt.buddy", {
  props: ["seed", "act", "since"],
  events: [],
});

const stage = empty_stage();
/** 舞台变一次加一，头像据此重画。 */
const changed = signal(0);
let seats = new Map<string, string>();
/** 收到过目录的机器。之后它的目录里新冒出来的会话是刚建的。 */
let known = new Set<string>();

function session_ids(conversation: any): string[] {
  const ids: string[] = [];
  if (conversation.acp) ids.push(conversation.acp.sid);
  for (const terminal of conversation.terminals ?? []) {
    if (terminal.id != null) ids.push(terminal.id);
  }
  return ids;
}

function sync(state: any) {
  const viewed_session = state.active_context.session;
  const next = new Map<string, string>();
  let viewed: string | null = null;
  const fresh: string[] = [];
  const machines = new Set<string>();
  for (const { machine, directory, facts } of state.machines) {
    if (!directory) continue;
    const seen = known.has(machine ?? "");
    for (const fact of facts) {
      const key = conversation_key(machine, fact.id);
      if (seen && !seats.has(key) && !stage.actors.has(key)) fresh.push(key);
      next.set(key, fact.status);
    }
    if (viewed_session && viewed_session.machine === machine) {
      const conversation = directory.conversations.find((conversation: any) =>
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
  changed.set(changed() + 1);
}

function same_seats(a: Map<string, string>, b: Map<string, string>) {
  if (a.size !== b.size) return false;
  for (const [key, status] of a) if (b.get(key) !== status) return false;
  return true;
}

let revision = -1;
function receive(state: any) {
  if (state.revision <= revision) return;
  revision = state.revision;
  sync(state);
}
host.on("smelt.state", receive);
host.call("state").then(receive, (error: unknown) => console.error(`[buddies] 取不到宿主状态：${error}`));

export default function Avatar(props: { args?: any }) {
  const { machine, conversation } = props.args ?? {};
  if (typeof conversation !== "string") {
    console.error(`[buddies] 头像的参数不对：${JSON.stringify(props.args)}`);
    return null;
  }
  const key = conversation_key(machine, conversation);
  const actor = () => {
    changed();
    return stage.actors.get(key);
  };
  return (
    <View style={{ width: CELL, height: CELL, flexShrink: 0 }}>
      <Buddy
        seed={conversation}
        act={actor()?.act ?? "rest"}
        since={actor()?.since ?? 0}
        style={{ width: CELL, height: CELL }}
      />
      {/* 原生组件自己吃指针；它只是画，盖一层，点击和悬停才算摆它的那一行的。 */}
      <View style={{ position: "absolute", top: 0, left: 0, width: CELL, height: CELL }} />
    </View>
  );
}
