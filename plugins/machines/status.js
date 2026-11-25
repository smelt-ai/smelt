// 状态写成字、要不要提醒、右键菜单项构造：只看数据，不碰界面，好测。

import {
  AGENT_ACCESS,
  EDIT_RELAY,
  INBOUND,
  RECONNECT,
  REMOVE,
  RENAME,
  UPGRADE,
} from "./contract.js";

/**
 * 走哪条路、多慢。
 */
export function link_summary(link, t) {
  const route = link?.route;
  if (route?.kind === "direct") {
    return t("link.direct", { ms: link.rtt_ms });
  }
  if (route?.kind === "relay") {
    return t("link.relay", { relay: route.relay, ms: link.rtt_ms });
  }
  return `${link.rtt_ms}ms`;
}

/**
 * 设置页机器一行下面那行字：在线写版本、平台、走哪条路多慢（慢了标黄），别的写为什么。
 * @returns {{ text: string, tone: "muted" | "ok" | "warning" | "bad" }}
 */
export function machine_status(connection, t) {
  const st = connection?.state;
  switch (st?.state) {
    case "connecting":
      return { text: t("machine.connecting"), tone: "muted" };
    case "online": {
      let line = st.platform
        ? `${t("online")} · ${st.version} · ${st.platform}`
        : `${t("online")} · ${st.version}`;
      let tone = "muted";
      if (connection.link) {
        line = `${line} · ${link_summary(connection.link, t)}`;
        if (connection.link.slow) {
          tone = "warning";
        }
      }
      return { text: line, tone };
    }
    case "offline":
      return { text: t("machine.offline", { error: st.error }), tone: "muted" };
    case "incompatible":
      return { text: t("machine.incompatible", { detail: st.detail }), tone: "muted" };
    case "upgrading":
      return { text: t("machine.upgrading", { progress: st.progress }), tone: "muted" };
    default:
      return { text: "", tone: "muted" };
  }
}

/**
 * 有要你处理的：机器表读不出来，或者哪台离线、版本不对、在线却慢。连接中、升级中是过程，
 * 不算。设置里「远程」页的角标看它。
 */
export function needs_attention(connections) {
  if (!connections) return false;
  if (connections.load_error != null) return true;
  const remotes = Array.isArray(connections.remotes) ? connections.remotes : [];
  return remotes.some((connection) => {
    const kind = connection?.state?.state;
    if (kind === "offline" || kind === "incompatible") return true;
    if (kind === "online") return Boolean(connection?.link?.slow);
    return false;
  });
}

/**
 * 长 id 留头尾各 8 个字。
 */
export function short_id(id) {
  const chars = Array.from(String(id ?? ""));
  if (chars.length <= 16) return chars.join("");
  return `${chars.slice(0, 8).join("")}…${chars.slice(chars.length - 8).join("")}`;
}

/**
 * 远程访问开关旁边那行字。
 * @returns {{ text: string, tone: "muted" | "ok" | "warning" | "bad" }}
 */
export function access_status(state, t) {
  switch (state?.state) {
    case "starting":
      return { text: t("access.starting"), tone: "muted" };
    case "running":
      return {
        text: t("access.running", {
          relay: state.relay ?? t("access.public_relay"),
          id: short_id(state.endpoint_id),
        }),
        tone: "ok",
      };
    case "failed":
      return { text: t("access.failed", { error: state.error }), tone: "bad" };
    case "off":
    default:
      return { text: t("access.off"), tone: "muted" };
  }
}

/**
 * 设备上次什么时候连过。`nowMs` 是现在的毫秒时间戳。
 */
export function last_seen_text(device, nowMs, t) {
  if (device?.connected) return t("online");
  const at = device?.last_seen_at_ms;
  if (at == null) return t("seen.never");
  const secs = Math.floor(Math.max(0, nowMs - at) / 1000);
  if (secs < 3600) {
    const mins = Math.max(1, Math.floor(secs / 60));
    return mins === 1 ? t("seen.minutes.one") : t("seen.minutes.other", { count: mins });
  }
  if (secs < 86_400) {
    const hours = Math.floor(secs / 3600);
    return hours === 1 ? t("seen.hours.one") : t("seen.hours.other", { count: hours });
  }
  const days = Math.floor(secs / 86_400);
  return days === 1 ? t("seen.days.one") : t("seen.days.other", { count: days });
}

/**
 * 会话列表机器行右键菜单项：按连接表顺序生成，每台远程机器一组。
 */
export function machine_menu_items(connections, textFn) {
  const remotes = Array.isArray(connections?.remotes) ? connections.remotes : [];
  const items = [];
  for (const conn of remotes) {
    const make = (labelKey, icon, command) => ({
      label: textFn(labelKey),
      icon,
      machine: conn.machine,
      command,
      args: null,
    });
    if (conn.can_reconnect) {
      items.push(make("menu.reconnect", "icons/refresh.svg", RECONNECT));
    }
    if (conn.can_upgrade) {
      items.push(make("menu.upgrade", "icons/arrow-up.svg", UPGRADE));
    }
    // 本机智能体经 `smelt --machine` 用它，默认不许；文字说的是点了会怎样。null 是还没迁到
    // 本机运行时的旧直连，没有这项。
    if (typeof conn.agent_access === "boolean") {
      const key = conn.agent_access ? "menu.disallow_agents" : "menu.allow_agents";
      items.push(make(key, "icons/bot.svg", AGENT_ACCESS));
    }
    // 它能不能连进来操作这台电脑，默认不许；不许时它只能回这边发过去的消息。
    if (typeof conn.inbound === "boolean") {
      const key = conn.inbound ? "menu.disallow_inbound" : "menu.allow_inbound";
      items.push(make(key, "icons/arrow-down.svg", INBOUND));
    }
    items.push(make("menu.rename", "icons/square-pen.svg", RENAME));
    items.push(make("menu.relay", "icons/globe.svg", EDIT_RELAY));
    items.push(make("menu.remove", "icons/trash.svg", REMOVE));
  }
  return items;
}
