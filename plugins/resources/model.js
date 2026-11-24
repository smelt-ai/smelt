// 资源占用纯领域逻辑：采样聚合、时间轴曲线、按对话归拢会话、进程摘要、排序、
// 机器快照构造、数值格式化与面积图 SVG 路径生成。
// 零 NSH / 宿主依赖，供 `bun test` 直接单测。

/** 两轮采样的间隔（毫秒）。 */
export const INTERVAL_MS = 2_000;
/** 刚开始看时先垫一轮，隔这么久再采第一轮真的。 */
export const PRIME_DELAY_MS = 500;
/** 单台机器问询超时（毫秒），一台慢不拖别的机器。 */
export const CALL_TIMEOUT_MS = 5_000;
/** 曲线点数；点距为 INTERVAL_MS，共覆盖两分钟。 */
export const CHART_POINTS = 60;

export const SORT_MEMORY = "memory";
export const SORT_CPU = "cpu";

const DEFAULT_TEXTS = {
  loading: "Loading…",
  "row.desktop": "Smelt app",
  "row.desktop_detail": "GPUI main process",
  "row.daemon": "smeltd background service",
  "memory.graphics": "Frame buffers and textures",
  "memory.graphics_detail": "Used for GPU drawing; bigger windows use more",
  "memory.heap": "App data",
  "memory.heap_detail":
    "Session content, text layout caches and so on — the part of the heap in use",
  "memory.other": "Other",
  "memory.other_detail": "System library data, thread stacks, free pages in the heap",
  "others.one": "1 session outside the sidebar",
  "others.other": "%{count} sessions outside the sidebar",
  "processes.one": "1 process",
  "processes.other": "%{count} processes",
  "processes.separator": ", ",
  "processes.and_more": "%{names}, …",
  "processes.named": "%{processes}: %{names}",
  "fallback.conversation": "Conversation",
  "fallback.terminal": "Terminal",
};

function defaultT(key, params) {
  let out = DEFAULT_TEXTS[key] ?? key;
  if (params && typeof out === "string") {
    for (const [k, v] of Object.entries(params)) {
      out = out.replaceAll(`%{${k}}`, String(v ?? ""));
    }
  }
  return out;
}

function emptyUsage() {
  return {
    cpu_percent: 0,
    memory_bytes: 0,
    process_count: 0,
  };
}

function cloneUsage(usage) {
  if (!usage) return emptyUsage();
  return {
    cpu_percent: Number(usage.cpu_percent ?? 0),
    memory_bytes: Number(usage.memory_bytes ?? 0),
    process_count: Number(usage.process_count ?? 0),
  };
}

function addUsage(total, usage) {
  return {
    cpu_percent: total.cpu_percent + Number(usage?.cpu_percent ?? 0),
    memory_bytes: total.memory_bytes + Number(usage?.memory_bytes ?? 0),
    process_count: total.process_count + Number(usage?.process_count ?? 0),
  };
}

/** 一台运行时的合计：守护进程和全部会话。 */
function runtimeTotal(result) {
  let total = cloneUsage(result?.daemon);
  for (const session of result?.sessions ?? []) {
    total = addUsage(total, session?.usage);
  }
  return total;
}

function findRuntimeResult(monitor, machine) {
  const key = machine ?? null;
  for (const entry of monitor?.runtimes ?? []) {
    if ((entry?.machine ?? null) === key) {
      return entry;
    }
  }
  return null;
}

function machineIdPrefix(machine) {
  return machine == null ? "local" : `remote:${machine}`;
}

/** 创建资源监视器状态：最新一轮样本与本机内存历史。 */
export function createMonitor() {
  return {
    desktop: null,
    desktopMemory: null,
    runtimes: [],
    history: [],
    chart: [],
  };
}

/** 重新开始看：上次看到的数和曲线都作废。 */
export function resetMonitor(monitor) {
  monitor.desktop = null;
  monitor.desktopMemory = null;
  monitor.runtimes = [];
  monitor.history = [];
  monitor.chart = [];
  return monitor;
}

/** 本机合计：桌面进程、本机守护和本机会话。本机运行时没回话时只有桌面。 */
export function localTotal(monitor) {
  if (!monitor?.desktop) return null;
  let total = cloneUsage(monitor.desktop);
  const local = findRuntimeResult(monitor, null);
  if (local?.ok && local.result) {
    total = addUsage(total, runtimeTotal(local.result));
  }
  return total;
}

/**
 * 曲线的点：从 `nowMs` 往回每隔 `intervalMs` 取一刻，取那一刻之前最近的样本，老的在前。
 * 还没有样本的时刻不出点：刚打开时点少，画出来铺满整宽，只是覆盖的时间短。
 */
export function memorySeries(
  history,
  nowMs,
  intervalMs = INTERVAL_MS,
  chartPoints = CHART_POINTS,
) {
  const out = [];
  if (!Array.isArray(history) || history.length === 0) {
    return out;
  }
  for (let back = chartPoints - 1; back >= 0; back -= 1) {
    const at = nowMs - intervalMs * back;
    for (let i = history.length - 1; i >= 0; i -= 1) {
      if (history[i].atMs <= at) {
        out.push(Number(history[i].bytes));
        break;
      }
    }
  }
  return out;
}

/** 写入一轮采样结果并刷新两分钟时间轴历史与曲线。 */
export function applySample(monitor, desktopSample, runtimes, nowMs = Date.now()) {
  if (desktopSample != null) {
    monitor.desktop = cloneUsage(desktopSample);
    monitor.desktopMemory = desktopSample.memory_breakdown ?? null;
  } else {
    monitor.desktop = null;
    monitor.desktopMemory = null;
  }
  monitor.runtimes = Array.isArray(runtimes) ? runtimes : [];

  const total = localTotal(monitor);
  if (total != null) {
    monitor.history.push({ atMs: nowMs, bytes: total.memory_bytes });
  }

  // 比曲线左端再早一点的样本还得留着：左端那一刻取的是它。
  const keepMs = INTERVAL_MS * (CHART_POINTS + 1);
  while (monitor.history.length > 1 && nowMs - monitor.history[0].atMs > keepMs) {
    monitor.history.shift();
  }
  monitor.chart = memorySeries(monitor.history, nowMs);
  return monitor;
}

function absorbSession(group, session) {
  group.usage = addUsage(group.usage, session?.usage);
  for (const proc of session?.processes ?? []) {
    group.processes.push({
      pid: Number(proc.pid ?? 0),
      name: String(proc.name ?? ""),
      command: String(proc.command ?? ""),
      usage: cloneUsage(proc.usage),
    });
  }
  group.processes.sort((a, b) => b.usage.memory_bytes - a.usage.memory_bytes);
}

/**
 * 把运行时报来的会话按侧栏会话归拢。
 * 对不上的（自动化、子智能体、别的客户端开的）合成一行放在最后。
 */
export function groupSessions(sessions, ownerLookup, tr = defaultT) {
  const lookup =
    typeof ownerLookup === "function"
      ? ownerLookup
      : (id) => (ownerLookup instanceof Map ? ownerLookup.get(id) : ownerLookup?.[id]);

  const groups = [];
  let others = null;
  let othersCount = 0;

  for (const session of sessions ?? []) {
    const found = lookup(session.session_id);
    let group;
    if (found != null) {
      const ownerIx = Array.isArray(found) ? found[0] : found.index;
      const ownerTitle = Array.isArray(found) ? found[1] : found.title;
      const existing = groups.find((item) => item.ownerIx === ownerIx);
      if (existing) {
        group = existing.group;
      } else {
        group = {
          title: ownerTitle,
          usage: emptyUsage(),
          processes: [],
          jump: session.session_id,
        };
        groups.push({ ownerIx, group });
      }
    } else {
      if (!others) {
        others = {
          title: "",
          usage: emptyUsage(),
          processes: [],
          jump: null,
        };
      }
      othersCount += 1;
      group = others;
    }
    absorbSession(group, session);
  }

  if (others) {
    others.title =
      othersCount === 1
        ? tr("others.one")
        : tr("others.other", { count: othersCount });
  }

  return {
    groups: groups.map((item) => item.group),
    others,
  };
}

/** 「5 个进程：node ×3、bun、rg」。按名字归拢，占内存多的名字在前，最多点四个名。 */
export function processSummary(count, processes, tr = defaultT) {
  const NAMES = 4;
  const names = [];
  for (const proc of processes ?? []) {
    const name = String(proc?.name ?? "");
    const existing = names.find((entry) => entry.name === name);
    if (existing) {
      existing.seen += 1;
    } else {
      names.push({ name, seen: 1 });
    }
  }
  const procsText =
    count === 1 ? tr("processes.one") : tr("processes.other", { count });
  if (names.length === 0) {
    return procsText;
  }
  const sep = tr("processes.separator");
  let listed = names
    .slice(0, NAMES)
    .map(({ name, seen }) => (seen === 1 ? name : `${name} ×${seen}`))
    .join(sep);
  if (names.length > NAMES) {
    listed = tr("processes.and_more", { names: listed });
  }
  return tr("processes.named", { processes: procsText, names: listed });
}

/** 重的在前，返回新数组不修改入参。 */
export function sortGroups(groups, sortBy = SORT_MEMORY) {
  const list = [...(groups ?? [])];
  if (sortBy === SORT_CPU) {
    list.sort((a, b) => {
      const av = Number(a?.usage?.cpu_percent ?? 0);
      const bv = Number(b?.usage?.cpu_percent ?? 0);
      if (Number.isNaN(bv) && Number.isNaN(av)) return 0;
      if (Number.isNaN(bv)) return -1;
      if (Number.isNaN(av)) return 1;
      return bv - av;
    });
  } else {
    list.sort(
      (a, b) => Number(b?.usage?.memory_bytes ?? 0) - Number(a?.usage?.memory_bytes ?? 0),
    );
  }
  return list;
}

/**
 * 每个守护会话属于哪段对话：守护会话 id → `{ index, title }`。
 * 终端分屏每格一个守护会话，都归到同一段对话；归档了的不算。
 */
export function sessionOwners(conversations, states, tr = defaultT) {
  const stateById = new Map();
  for (const s of states ?? []) {
    if (s?.id) {
      stateById.set(s.id, s);
    }
  }
  const owners = new Map();
  const list = conversations ?? [];
  for (let ix = 0; ix < list.length; ix += 1) {
    const conv = list[ix];
    if (!conv || conv.archived_at != null) continue;
    let ids = [];
    let fallback = "";
    if (conv.acp != null && conv.acp.sid) {
      ids = [conv.acp.sid];
      fallback = tr("fallback.conversation");
    } else {
      ids = (conv.terminals ?? [])
        .map((terminal) => terminal?.id)
        .filter((id) => typeof id === "string" && id.length > 0);
      fallback = tr("fallback.terminal");
    }
    if (ids.length === 0) continue;

    let title = null;
    for (const id of ids) {
      const name = stateById.get(id)?.conversation_name;
      if (typeof name === "string") {
        title = name;
        break;
      }
    }
    const resolved = title ?? fallback;
    for (const id of ids) {
      owners.set(id, { index: ix, title: resolved });
    }
  }
  return owners;
}

/** 桌面进程的内存拆分，按固定顺序列，数字跳动时行不换位置。 */
export function memoryParts(memory, tr = defaultT) {
  if (!memory) return [];
  const graphics = Number(memory.graphics ?? 0);
  const heap = Number(memory.heap ?? 0);
  const other =
    memory.other != null
      ? Number(memory.other)
      : Math.max(0, Number(memory.total ?? 0) - graphics - heap);
  return [
    {
      id: "graphics",
      label: tr("memory.graphics"),
      detail: tr("memory.graphics_detail"),
      bytes: graphics,
    },
    {
      id: "heap",
      label: tr("memory.heap"),
      detail: tr("memory.heap_detail"),
      bytes: heap,
    },
    {
      id: "other",
      label: tr("memory.other"),
      detail: tr("memory.other_detail"),
      bytes: other,
    },
  ];
}

/** 拼出资源页要显示的全部内容：本机在前，远程机器按宿主顺序。 */
export function buildSnapshot(monitor, machineList, tr = defaultT) {
  const localRes = findRuntimeResult(monitor, null);
  const totalMemory =
    localRes?.ok && Number(localRes.result?.total_memory_bytes ?? 0) > 0
      ? Number(localRes.result.total_memory_bytes)
      : null;

  const sections = (machineList ?? []).map((machine) => {
    const machineKey = machine?.machine ?? null;
    const isLocal = machineKey === null;
    const idPrefix = machineIdPrefix(machineKey);
    const owners = sessionOwners(
      machine?.directory?.conversations ?? [],
      machine?.states ?? [],
      tr,
    );

    const fixed = [];
    let total = null;
    let sessions = [];
    let others = null;
    let note = null;

    if (isLocal && monitor?.desktop != null) {
      fixed.push({
        id: `${idPrefix}-fixed-${fixed.length}`,
        label: tr("row.desktop"),
        detail: tr("row.desktop_detail"),
        usage: cloneUsage(monitor.desktop),
        parts: memoryParts(monitor.desktopMemory, tr),
      });
      total = cloneUsage(monitor.desktop);
    }

    const res = findRuntimeResult(monitor, machineKey);
    if (res?.ok && res.result) {
      total = addUsage(total ?? emptyUsage(), runtimeTotal(res.result));
      fixed.push({
        id: `${idPrefix}-fixed-${fixed.length}`,
        label: tr("row.daemon"),
        detail: null,
        usage: cloneUsage(res.result.daemon),
        parts: [],
      });
      const grouped = groupSessions(res.result.sessions ?? [], owners, tr);
      sessions = grouped.groups;
      others = grouped.others;
    } else if (res && !res.ok) {
      note = String(res.error ?? "");
    } else {
      note = machine?.unavailable ?? tr("loading");
    }

    return {
      name: String(machine?.name ?? ""),
      idPrefix,
      machine: machineKey,
      total,
      fixed,
      sessions,
      others,
      note,
    };
  });

  return {
    local: localTotal(monitor),
    totalMemory,
    chart: [...(monitor?.chart ?? [])],
    sections,
  };
}

export function formatBytes(bytes) {
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  const value = Number(bytes ?? 0);
  if (value >= GB) {
    return `${(value / GB).toFixed(1)} GB`;
  }
  if (value >= MB) {
    return `${(value / MB).toFixed(0)} MB`;
  }
  return `${(value / KB).toFixed(0)} KB`;
}

export function formatCpu(percent) {
  const value = Number(percent ?? 0);
  if (value < 10) {
    return `${value.toFixed(1)}%`;
  }
  return `${value.toFixed(0)}%`;
}

function fmtCoord(n) {
  const rounded = Math.round(n * 100) / 100;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(2).replace(/0+$/, "");
}

function splitSvgColor(raw, baseOpacity) {
  const color = String(raw).trim();
  const m = color.match(
    /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)(?:\s*,\s*([\d.]+))?\s*\)$/i,
  );
  if (m) {
    const rgb = `rgb(${Math.round(Number(m[1]))},${Math.round(Number(m[2]))},${Math.round(Number(m[3]))})`;
    const alpha = m[4] != null ? Math.max(0, Math.min(1, Number(m[4]))) : 1;
    return { color: rgb, opacity: Math.round(alpha * baseOpacity * 1000) / 1000 };
  }
  return { color, opacity: Math.round(baseOpacity * 1000) / 1000 };
}

/**
 * 为 `<SmeltVector>` 生成无坐标轴的面积图内联 SVG：
 * 从左到右铺满宽度、底边为 0、单色描边 + 轻透填充；少于两个有效点或上限无效返回空串。
 * `stroke` 必填，取主题色板里的色。
 */
export function buildAreaChartSvg(values, options = {}) {
  if (!Array.isArray(values) || values.length < 2) {
    return "";
  }
  const width = Number(options.width ?? 600);
  const height = Number(options.height ?? 96);
  const strokeWidth = Number(options.strokeWidth ?? 1.5);
  if (!Number.isFinite(width) || width <= 0 || !Number.isFinite(height) || height <= 0) {
    return "";
  }

  let peak = 0;
  for (const v of values) {
    if (Number.isFinite(v) && v > peak) {
      peak = v;
    }
  }
  const max = options.max != null ? Number(options.max) : peak * 1.2;
  if (!Number.isFinite(max) || max <= 0) {
    return "";
  }

  const step = width / (values.length - 1);
  const segments = [];
  let current = [];
  for (let i = 0; i < values.length; i += 1) {
    const v = values[i];
    if (Number.isFinite(v)) {
      const ratio = Math.max(0, Math.min(1, v / max));
      const x = i * step;
      const y = height * (1 - ratio);
      current.push({ x, y });
    } else if (current.length > 0) {
      segments.push(current);
      current = [];
    }
  }
  if (current.length > 0) {
    segments.push(current);
  }

  const validSegments = segments.filter((seg) => seg.length >= 2);
  if (validSegments.length === 0) {
    return "";
  }

  const strokeSpec = splitSvgColor(options.stroke, Number(options.strokeOpacity ?? 0.9));
  const fillSpec = splitSvgColor(
    options.fill ?? options.stroke,
    Number(options.fillOpacity ?? 0.16),
  );

  const hStr = fmtCoord(height);
  const elements = [];
  for (const seg of validSegments) {
    const lineCmds = seg
      .map((pt, idx) => `${idx === 0 ? "M" : "L"}${fmtCoord(pt.x)} ${fmtCoord(pt.y)}`)
      .join(" ");
    const firstX = fmtCoord(seg[0].x);
    const lastX = fmtCoord(seg[seg.length - 1].x);
    const areaD = `${lineCmds} L${lastX} ${hStr} L${firstX} ${hStr} Z`;
    elements.push(
      `<path d="${areaD}" fill="${fillSpec.color}" fill-opacity="${fillSpec.opacity}" stroke="none"/>`,
    );
    elements.push(
      `<path d="${lineCmds}" fill="none" stroke="${strokeSpec.color}" stroke-opacity="${strokeSpec.opacity}" stroke-width="${fmtCoord(strokeWidth)}" stroke-linejoin="round" stroke-linecap="round"/>`,
    );
  }

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${fmtCoord(width)} ${hStr}" preserveAspectRatio="none">${elements.join("")}</svg>`;
}
