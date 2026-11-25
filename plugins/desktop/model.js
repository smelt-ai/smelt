// 远程桌面插件纯逻辑：重连退避、状态文案、机器桌面能力探测规划与菜单项生成（供 bun test 直测）。

export const RETRY_DELAYS_MS = [1000, 2000, 4000, 8000, 16000, 30000];

export function retryDelayMs(retries) {
  return retries >= 0 && retries < RETRY_DELAYS_MS.length ? RETRY_DELAYS_MS[retries] : null;
}

export function describeClosed(closed, t) {
  switch (closed?.kind) {
    case "proxy":
      return t("closed.proxy", { error: closed.error ?? "" });
    case "webview":
      return t("closed.webview", { error: closed.error ?? "" });
    case "by_user":
      return t("closed.by_user");
    case "ended":
      return t("closed.ended");
    case "lost":
      return t("closed.lost");
    default:
      return t("closed.lost");
  }
}

export function statusPresentation(status, controlling, typeError, t) {
  if (status?.kind === "connected") {
    if (typeError) {
      return {
        label: t("status.type_failed", { error: typeError }),
        tone: "bad",
      };
    }
    if (controlling) {
      return { label: t("status.controlling"), tone: "strong" };
    }
    return { label: t("status.viewing"), tone: "muted" };
  }
  if (status?.kind === "connecting") {
    return { label: t("status.connecting"), tone: "muted" };
  }
  if (status?.kind === "reconnecting") {
    return { label: t("status.reconnecting"), tone: "muted" };
  }
  if (status?.kind === "closed") {
    const tone = status.closed?.kind === "by_user" ? "muted" : "bad";
    return { label: describeClosed(status.closed, t), tone };
  }
  return { label: t("status.connecting"), tone: "muted" };
}

/**
 * 计算在线远程机器的桌面探测计划：
 * - 移除已离线或已删除的机器探测缓存；
 * - 对尚未探测的新上线远程机器返回需要发起 `system.describe` 的机器 id 列表。
 */
export function planMachineProbes(machines, probes) {
  const onlineRemotes = new Set();
  for (const m of machines ?? []) {
    if (typeof m?.machine === "string" && m.machine.length > 0 && m.online === true) {
      onlineRemotes.add(m.machine);
    }
  }
  let removed = false;
  for (const id of [...probes.keys()]) {
    if (!onlineRemotes.has(id)) {
      probes.delete(id);
      removed = true;
    }
  }
  const toAsk = [];
  for (const id of onlineRemotes) {
    if (!probes.has(id)) {
      toAsk.push(id);
    }
  }
  toAsk.sort();
  return { toAsk, removed };
}

/** 按探测结果生成注册给 `smelt.session-list/machine` 的右键菜单项列表。 */
export function buildMachineMenuItems(probes, titleText, openCommand) {
  const items = [];
  const ids = [...probes.keys()].sort();
  for (const id of ids) {
    const entry = probes.get(id);
    if (entry?.kind === "known" && entry.desktop === true) {
      items.push({
        label: titleText,
        icon: "icons/monitor.svg",
        machine: id,
        command: openCommand,
        args: null,
      });
    }
  }
  return items;
}
