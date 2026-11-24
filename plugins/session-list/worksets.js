// 工作树的合并情况：关闭前、窗口回到前台时问运行时（workspace.workset.inspect，会先 fetch 各
// 仓库的基线），结果缓存在 state.workset_checks 里给侧栏画。这里只放判断，问的动作在 ops.js。
//
// 什么时候问，全由事件决定，不起定时器：
// - 目录里出现了还没问过的工作树（sessions_changed）：问一次，侧栏才有状态可画。
// - 窗口回到前台（window_activated）：上次问过超过 RECHECK_AFTER 的再问。人常常是去浏览器合完
//   PR 回来，这时候该看到「已合并」。
// - 用户点「检查合并状态」、点「关闭工作树」：马上问。

/** 窗口回到前台时，离上次检查超过这么久（毫秒）才再查。查一次要 fetch 每个仓库。 */
export const RECHECK_AFTER = 5 * 60 * 1000;
/** 检查、关闭工作树的调用时限（毫秒）：要 fetch 每个仓库的远端，慢网络下可能要几分钟。 */
export const WORKSET_GIT_TIMEOUT = 30 * 60 * 1000;

/** 一棵工作树在 workset_checks 里的键。 */
export function workset_key(machine, id) {
  return JSON.stringify([machine ?? null, id]);
}

/**
 * 侧栏上能说的合并情况："unknown"（还没查过，或者查失败了）| "merged"（每个仓库都干净、分支都
 * 合进了基线：可以关）| "unconfirmed"（有仓库判断不了是否已合并，其余都没问题：确认后可以关）|
 * "pending"（有未提交改动或还没合并的分支）。
 *
 * check 是 { running, last: { at, result } | { at, error } | null }。
 */
export function merge_summary(check) {
  const result = check?.last?.result;
  return result ? merge_of_result(result) : "unknown";
}

export function merge_of_result(result) {
  const blocked = result.repositories.some(
    (repository) => repository.dirty || repository.merge === "unmerged",
  );
  if (blocked) return "pending";
  if (result.repositories.some((repository) => repository.merge === "unconfirmed")) return "unconfirmed";
  return "merged";
}

/** 给人看的每个仓库的情况，一行一个（悬停提示、确认框里用）。 */
export function describe(result, t) {
  const lines = result.repositories.map((repository) => {
    let state;
    if (repository.dirty) state = t("workset.dirty");
    else if (repository.merge === "unmerged") state = t("workset.unmerged");
    else if (repository.merge === "unconfirmed") state = t("workset.unconfirmed");
    else state = t("workset.merged");
    return t("workset.repository_state", { name: repository.name, state });
  });
  if ((result.refresh_failures?.length ?? 0) > 0) lines.push(t("workset.refresh_failed"));
  return lines;
}

/**
 * 目录里每棵工作树要不要问：stale 为真时连问过但超过 RECHECK_AFTER 的也问，否则只问还没问
 * 过的。present 是 [{ machine, id }]，checks 是 workset_key → check。
 */
export function due_checks(present, checks, stale, now) {
  return present.filter(({ machine, id }) => {
    const check = checks.get(workset_key(machine, id));
    if (!check) return true;
    if (check.running) return false;
    if (!check.last) return true;
    return stale && now - check.last.at >= RECHECK_AFTER;
  });
}
