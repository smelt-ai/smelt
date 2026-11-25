// 什么时候演什么：每段会话的头像在演哪一出、从什么时候起。
//
// 只在会话状态、露着的会话变了时（事件里）改：开始跑就干活，要你就喊，跑完欢呼一下，被选中时闲着
// 就打个招呼，已经收到过目录的机器上新冒出来的会话（刚建的）也打个招呼。第一次看到的会话（打开
// app、远端连上）只按状态摆，不起反应。演一遍就停的那几出不用定时器收尾：头像按时间自己演完站好。

/** 演一遍就停的那几出演多久（毫秒）：头像（nsh/avatar.tsx）按它演完站好。 */
export const ONE_SHOT_MS = { cheer: 1600, greet: 1100 };

/** 状态对应一直演的那出。 */
export function act_of(status) {
  switch (status) {
    case "running":
      return "work";
    case "needs_you":
      return "call";
    default:
      return "rest";
  }
}

/** 状态从 from 变成 to 时演哪一出。 */
export function act_on_change(from, to) {
  if (to === "needs_you") return "call";
  if (to === "running") return "work";
  return from === "running" ? "cheer" : "rest";
}

/** 机器上的一段会话在 Map、Set 里的键。 */
export function conversation_key(machine, id) {
  return JSON.stringify([machine ?? null, id]);
}

export function empty_stage() {
  return {
    /** 键 → { status, act, since }：since 是开演的 Unix 毫秒。 */
    actors: new Map(),
    /** 上一次看到的露着的会话，比出选中变了没有。 */
    selected: null,
  };
}

/** 闲着：站着，或演一遍的那出已经演完。 */
function idle(actor, now) {
  const length = ONE_SHOT_MS[actor.act];
  return length != null ? now - actor.since >= length : actor.act === "rest";
}

/**
 * 会话状态或选中变了：状态变了的换一出，新来的按状态摆，没了的去掉；刚选中的闲着就打个招呼。
 * rows 是所有会话的键 → 状态。
 */
export function track(stage, rows, selected, now) {
  for (const [key, status] of rows) {
    const actor = stage.actors.get(key);
    let act;
    if (!actor) act = act_of(status);
    else if (actor.status !== status) act = act_on_change(actor.status, status);
    else continue;
    stage.actors.set(key, { status, act, since: now });
  }
  if (stage.actors.size !== rows.size) {
    for (const key of stage.actors.keys()) if (!rows.has(key)) stage.actors.delete(key);
  }
  if (stage.selected !== selected) {
    stage.selected = selected;
    if (selected != null) greet(stage, selected, now);
  }
}

/** key 闲着就打个招呼（被选中、刚新建出来）。 */
export function greet(stage, key, now) {
  const actor = stage.actors.get(key);
  if (actor && idle(actor, now)) stage.actors.set(key, { ...actor, act: "greet", since: now });
}
