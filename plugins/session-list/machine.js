// 机器行上的系统图标和连接指示：一盏灯（图标的颜色），悬停图标看完整说明。只读宿主给的机器表
// （machines）和连接表（connections）。

/**
 * 一台机器眼下连得怎样：
 * - light：灯，"good"（在线、延迟正常）| "degraded"（能用但要留意：连接中、升级中、延迟高）|
 *   "down"（用不了：离线、版本不兼容）。系统图标就画成这个色。
 * - detail：悬停机器图标时的完整说明。
 * - unavailable：用不了时会话行尾写的短字；能用是 null。
 *
 * 本机不在连接表里，只看机器表里的 unavailable。
 */
export function link(machine, connection, t) {
  if (connection) return remote_link(connection.state, connection.link, t);
  if (machine.unavailable != null) {
    return {
      light: "down",
      detail: t("machine.offline_detail", { reason: machine.unavailable }),
      unavailable: t("machine.offline"),
    };
  }
  return { light: "good", detail: t("machine.online"), unavailable: null };
}

export function remote_link(state, link, t) {
  let light;
  let unavailable = null;
  let detail;
  switch (state.state) {
    case "online":
      light = link?.slow ? "degraded" : "good";
      if (!link) detail = t("machine.online");
      else if (link.route.kind === "other") detail = t("machine.online_rtt", { rtt: link.rtt_ms });
      else detail = link_summary(link, t);
      break;
    case "connecting":
      light = "degraded";
      unavailable = t("machine.offline");
      detail = t("machine.connecting");
      break;
    case "upgrading":
      light = "degraded";
      unavailable = t("machine.upgrading");
      detail = state.progress;
      break;
    case "offline":
      light = "down";
      unavailable = t("machine.offline");
      detail = t("machine.offline_detail", { reason: state.error });
      break;
    case "incompatible": {
      light = "down";
      unavailable = t("machine.incompatible");
      // 连得上，连接路径照样有：也说走哪条路、多慢；悬停还要看得到为什么、该升级哪边。
      detail = t("machine.incompatible_detail", { version: state.version });
      if (link) detail += ` · ${link_summary(link, t)}`;
      if (state.detail) detail += `\n${state.detail}`;
      break;
    }
  }
  return { light, detail, unavailable };
}

/** 走哪条路、多慢。 */
function link_summary(link, t) {
  switch (link.route.kind) {
    case "direct":
      return t("machine.direct", { rtt: link.rtt_ms });
    case "relay":
      return t("machine.relay", { relay: link.route.relay, rtt: link.rtt_ms });
    default:
      return `${link.rtt_ms}ms`;
  }
}

/**
 * 机器行的系统图标。本机按这台电脑的系统（local_os，os.platform() 的写法）；远程按宿主记着的
 * 系统，离线也认得；从没连上过的是通用的笔记本。
 */
export function os_icon(machine, connections, local_os) {
  const os =
    machine == null
      ? local_os
      : connections.remotes.find((connection) => connection.machine === machine)?.os;
  return icon_for_os(os);
}

export function icon_for_os(os) {
  switch ((os ?? "").trim().toLowerCase()) {
    case "macos":
    case "darwin":
      return "icons/apple.svg";
    case "linux":
      return "icons/linux.svg";
    case "windows":
    case "win32":
      return "icons/windows.svg";
    default:
      return "icons/laptop.svg";
  }
}
