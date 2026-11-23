// 维护插件纯逻辑：字节格式化、残留汇总与 macOS CLI 状态文案映射（无 NSH 依赖，供 bun test 直测）。

export function formatBytes(bytes) {
  const value = Number(bytes) || 0;
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  if (value >= GB) return `${(value / GB).toFixed(1)} GB`;
  if (value >= MB) return `${(value / MB).toFixed(1)} MB`;
  if (value >= KB) return `${(value / KB).toFixed(0)} KB`;
  return `${Math.max(0, Math.trunc(value))} B`;
}

export function totalResidueBytes(items) {
  let sum = 0;
  for (const item of items ?? []) {
    sum += Number(item?.bytes) || 0;
  }
  return sum;
}

export function cleanupSummary(scan, t) {
  if (!scan || scan.kind === "running") return t("cleanup.scanning");
  if (scan.kind === "failed") return t("cleanup.scan_failed", { error: scan.error ?? "" });
  const items = scan.items ?? [];
  if (items.length === 0) return t("cleanup.empty");
  return t("cleanup.summary", {
    count: items.length,
    size: formatBytes(totalResidueBytes(items)),
  });
}

export function cliStatusDescription(status, link, t) {
  const path = link || "/usr/local/bin/smelt";
  switch (status) {
    case "installed":
      return t("cli.status.installed", { link: path });
    case "missing":
      return t("cli.status.missing");
    case "stale":
      return t("cli.status.stale", { link: path });
    case "taken":
      return t("cli.status.taken", { link: path });
    default:
      return t("cli.status.unknown");
  }
}

export function cliActionLabel(status, installing, t) {
  if (installing) return t("cli.install.running");
  if (status === "installed") return t("cli.install.again");
  return t("cli.install.action");
}

export function cliInstallErrorReason(result, t) {
  const link = result?.link || "/usr/local/bin/smelt";
  const raw = String(result?.error ?? "");
  switch (result?.code) {
    case "taken":
      return t("cli.status.taken", { link });
    case "cancelled":
      return t("cli.install.cancelled");
    case "no_home":
      return t("cli.install.no_home");
    case "link_failed":
      return t("cli.install.link_failed", { error: raw });
    case "osascript_failed":
      return t("cli.install.osascript_failed", { error: raw });
    default:
      return raw || "unknown error";
  }
}
