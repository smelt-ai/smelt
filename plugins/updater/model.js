// 更新与关于页纯展示模型（无 NSH 依赖，供 bun test 直测）。

export function channelLabel(channel, t) {
  return channel === "dev" ? t("channel.dev") : t("channel.prod");
}

export function formatVersionHeader(version, supported, channel, t) {
  const base = `v${version || "0.0.0"}`;
  if (!supported || !channel) return base;
  return `${base} · ${channelLabel(channel, t)}`;
}

export function formatMb(bytes) {
  return ((Number(bytes) || 0) / 1024 / 1024).toFixed(1);
}

export function statusPresentation(snapshot, t) {
  const status = snapshot?.status ?? { kind: "idle" };
  const ch = channelLabel(snapshot?.channel, t);
  switch (status.kind) {
    case "checking":
      return {
        title: t("status.checking"),
        detail: t("status.checking_detail", { channel: ch }),
        tone: "run",
      };
    case "up_to_date":
      return {
        title: t("status.up_to_date"),
        detail: t("status.up_to_date_detail", { channel: ch }),
        tone: "ok",
      };
    case "available":
      return {
        title: t("status.available"),
        detail: t("status.available_detail", {
          channel: ch,
          version: status.version ?? "",
        }),
        tone: "run",
      };
    case "downloading": {
      const received = Number(status.received) || 0;
      const total = Number(status.total) || 0;
      const detail =
        total > 0
          ? t("status.downloading_progress", {
              version: status.version ?? "",
              percent: Math.round((received / total) * 100).toFixed(0),
              received: formatMb(received),
              total: formatMb(total),
            })
          : t("status.downloading_bytes", {
              version: status.version ?? "",
              received: formatMb(received),
            });
      return {
        title: t("status.downloading"),
        detail,
        tone: "run",
      };
    }
    case "installing":
      return {
        title: t("status.installing"),
        detail: t("status.installing_detail", { version: status.version ?? "" }),
        tone: "run",
      };
    case "applying":
      return {
        title: t("status.applying"),
        detail: t("status.applying_detail", { version: status.version ?? "" }),
        tone: "run",
      };
    case "ready":
      return {
        title: t("status.ready"),
        detail: t("status.ready_detail", { version: status.version ?? "" }),
        tone: "ok",
      };
    case "install_failed":
      return {
        title: t("status.install_failed"),
        detail: t("status.install_failed_detail", { version: status.version ?? "" }),
        tone: "bad",
      };
    case "failed": {
      const code = status.failure ?? "check";
      const title = t(`failure.${code}`);
      const detail =
        code === "location" && snapshot?.move_app_error
          ? String(snapshot.move_app_error)
          : t(`failure.${code}_detail`);
      return { title, detail, tone: "bad" };
    }
    default:
      return {
        title: t("status.idle"),
        detail: t("status.idle_detail", { channel: ch }),
        tone: "muted",
      };
  }
}

export function checkButtonLabel(status, t) {
  switch (status?.kind) {
    case "checking":
      return t("check.checking");
    case "downloading":
      return t("check.downloading");
    case "installing":
      return t("check.installing");
    case "applying":
      return t("check.quitting");
    case "ready":
    case "install_failed":
      return t("check.again");
    case "failed":
      return status.failure === "recovery" ? t("check.retry_recovery") : t("check.idle");
    default:
      return t("check.idle");
  }
}

export function progressModel(status) {
  if (!status) return null;
  if (status.kind === "downloading") {
    const total = Number(status.total) || 0;
    if (total > 0) {
      const pct = Math.max(0, Math.min(100, ((Number(status.received) || 0) / total) * 100));
      return { kind: "determinate", percent: pct };
    }
    return { kind: "indeterminate" };
  }
  if (status.kind === "installing" || status.kind === "applying") {
    return { kind: "indeterminate" };
  }
  return null;
}
