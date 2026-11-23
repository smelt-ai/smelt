import { describe, expect, test } from "bun:test";
import {
  applySample,
  buildAreaChartSvg,
  buildSnapshot,
  createMonitor,
  formatBytes,
  formatCpu,
  groupSessions,
  localTotal,
  memorySeries,
  processSummary,
  resetMonitor,
  sessionOwners,
  SORT_CPU,
  SORT_MEMORY,
  sortGroups,
} from "../model.js";

function usage(cpu_percent, memory_bytes, process_count) {
  return { cpu_percent, memory_bytes, process_count };
}

function processEntry(name, memory_bytes) {
  return {
    pid: 1,
    name,
    command: "",
    usage: usage(0, memory_bytes, 1),
  };
}

function sessionUsage(id, u) {
  return {
    session_id: id,
    usage: u,
    processes: [processEntry(id, u.memory_bytes)],
  };
}

describe("smelt.resources monitor & model", () => {
  test("panes of one session add up and unknown sessions are pooled", () => {
    const sessions = [
      sessionUsage("pane-a", usage(10, 100, 2)),
      sessionUsage("pane-b", usage(5, 150, 1)),
      sessionUsage("chat", usage(1, 300, 4)),
      sessionUsage("automation", usage(2, 20, 1)),
      sessionUsage("subagent", usage(3, 30, 1)),
    ];
    const { groups, others } = groupSessions(sessions, (id) => {
      if (id === "pane-a" || id === "pane-b") return [0, "终端"];
      if (id === "chat") return [1, "对话"];
      return null;
    });
    expect(groups).toEqual([
      {
        title: "终端",
        usage: usage(15, 250, 3),
        // 两个 pane 的进程合在一起，内存大的在前。
        processes: [processEntry("pane-b", 150), processEntry("pane-a", 100)],
        jump: "pane-a",
      },
      {
        title: "对话",
        usage: usage(1, 300, 4),
        processes: [processEntry("chat", 300)],
        jump: "chat",
      },
    ]);
    expect(others).toEqual({
      title: "2 sessions outside the sidebar",
      usage: usage(5, 50, 2),
      processes: [processEntry("subagent", 30), processEntry("automation", 20)],
      jump: null,
    });
  });

  test("the summary names the heaviest processes", () => {
    const processes = [
      processEntry("node", 500),
      processEntry("bun", 400),
      processEntry("node", 300),
      processEntry("rg", 20),
      processEntry("node", 10),
    ];
    expect(processSummary(5, processes)).toBe("5 processes: node ×3, bun, rg");
    const many = ["a", "b", "c", "d", "e"].map((name) => processEntry(name, 1));
    expect(processSummary(9, many)).toBe("9 processes: a, b, c, d, …");
    expect(processSummary(3, [])).toBe("3 processes");
    expect(processSummary(1, [processEntry("zsh", 10)])).toBe("1 process: zsh");
  });

  test("sorting puts the heaviest first", () => {
    const group = (title, cpu, memory) => ({
      title,
      usage: usage(cpu, memory, 1),
      processes: [],
      jump: title,
    });
    const groups = [group("a", 50, 10), group("b", 1, 900)];
    const byMem = sortGroups(groups, SORT_MEMORY);
    expect(byMem[0].title).toBe("b");
    const byCpu = sortGroups(groups, SORT_CPU);
    expect(byCpu[0].title).toBe("a");
    // 原数组不被修改
    expect(groups[0].title).toBe("a");
  });

  test("the chart follows time not sample count", () => {
    const start = 1_000_000;
    const history = [
      { atMs: start, bytes: 100 },
      { atMs: start + 10_000, bytes: 200 },
    ];
    const now = start + 14_000;
    const series = memorySeries(history, now);
    // now 往回到 start 共 8 个点（0、2、…、14 秒），前 5 个取第一份样本。
    expect(series).toEqual([100, 100, 100, 100, 100, 200, 200, 200]);
  });

  test("old samples are dropped but the one feeding the left edge stays", () => {
    const start = 1_000_000;
    const monitor = createMonitor();
    applySample(monitor, { ...usage(1, 10, 1), memory_breakdown: null }, [], start);
    const later = start + 600_000;
    applySample(monitor, { ...usage(1, 20, 1), memory_breakdown: null }, [], later);
    expect(monitor.history.length).toBe(1);
    expect(monitor.chart).toEqual([20]);

    // 刚好在保留窗口内的样本（左端需要用到它）不会被删
    const edgeMonitor = createMonitor();
    applySample(edgeMonitor, { ...usage(1, 10, 1), memory_breakdown: null }, [], start);
    applySample(edgeMonitor, { ...usage(1, 20, 1), memory_breakdown: null }, [], start + 120_000);
    expect(edgeMonitor.history.length).toBe(2);
    expect(edgeMonitor.chart.length).toBe(60);
    expect(edgeMonitor.chart[0]).toBe(10);
    expect(edgeMonitor.chart[59]).toBe(20);
  });

  test("coming back starts from a clean slate", () => {
    const start = 1_000_000;
    const monitor = createMonitor();
    applySample(monitor, { ...usage(1, 10, 1), memory_breakdown: null }, [], start);
    resetMonitor(monitor);
    expect(localTotal(monitor)).toBeNull();
    expect(monitor.history).toEqual([]);
    expect(monitor.chart).toEqual([]);
    applySample(monitor, { ...usage(1, 30, 1), memory_breakdown: null }, [], start + 300_000);
    expect(monitor.chart).toEqual([30]);
  });

  test("the local total covers the desktop daemon and sessions", () => {
    const monitor = createMonitor();
    const localResult = {
      daemon: usage(1, 10, 1),
      sessions: [sessionUsage("s", usage(2, 20, 3))],
      total_memory_bytes: 1000,
    };
    applySample(
      monitor,
      { ...usage(4, 40, 1), memory_breakdown: null },
      [{ machine: null, ok: true, result: localResult }],
      1_000_000,
    );
    expect(localTotal(monitor)).toEqual(usage(7, 70, 5));
  });

  test("owners follow the directory conversations", () => {
    const terminal = (id) => ({ id });
    const conversations = [
      {
        custom_title: "旧的",
        archived_at: 1,
        acp: { sid: "archived" },
        terminals: [],
      },
      {
        archived_at: null,
        acp: { sid: "acp" },
        terminals: [],
      },
      {
        archived_at: null,
        acp: null,
        terminals: [terminal("a"), terminal(null), terminal("b")],
      },
      {
        archived_at: null,
        acp: null,
        terminals: [terminal(null)],
      },
    ];
    const named = (id, name) => ({ id, conversation_name: name });
    const owners = sessionOwners(conversations, [named("b", "分屏")]);
    expect(owners.size).toBe(3);
    expect(owners.has("archived")).toBe(false);
    // 还没推名字来：通用名。
    expect(owners.get("acp")).toEqual({ index: 1, title: "Conversation" });
    // 哪格报上来都认。
    expect(owners.get("a")).toEqual({ index: 2, title: "分屏" });
    expect(owners.get("b")).toEqual({ index: 2, title: "分屏" });
  });

  test("numbers read like Activity Monitor", () => {
    expect(formatBytes(512 * 1024)).toBe("512 KB");
    expect(formatBytes(820 * 1024 * 1024)).toBe("820 MB");
    expect(formatBytes(1288 * 1024 * 1024)).toBe("1.3 GB");
    expect(formatCpu(0.34)).toBe("0.3%");
    expect(formatCpu(135.4)).toBe("135%");
  });

  test("buildSnapshot assembles local breakdown, daemon, sessions, and remote notes", () => {
    const monitor = createMonitor();
    applySample(
      monitor,
      {
        cpu_percent: 12.5,
        memory_bytes: 500,
        process_count: 1,
        memory_breakdown: { total: 500, graphics: 200, heap: 180, other: 120 },
      },
      [
        {
          machine: null,
          ok: true,
          result: {
            daemon: usage(1.5, 100, 1),
            sessions: [
              sessionUsage("acp-1", usage(3, 300, 2)),
              sessionUsage("extra-1", usage(1, 50, 1)),
            ],
            total_memory_bytes: 16_000,
          },
        },
        {
          machine: "box-err",
          ok: false,
          error: "timeout",
        },
      ],
      2_000_000,
    );

    const snap = buildSnapshot(monitor, [
      {
        machine: null,
        name: "This Mac",
        unavailable: null,
        directory: {
          conversations: [{ archived_at: null, acp: { sid: "acp-1" }, terminals: [] }],
        },
        states: [{ id: "acp-1", conversation_name: "Fix bug" }],
      },
      {
        machine: "box-err",
        name: "Build Box",
        unavailable: null,
        directory: null,
        states: [],
      },
      {
        machine: "box-off",
        name: "Offline Box",
        unavailable: "Offline: connection refused",
        directory: null,
        states: [],
      },
      {
        machine: "box-wait",
        name: "Loading Box",
        unavailable: null,
        directory: null,
        states: [],
      },
    ]);

    expect(snap.local).toEqual(usage(18, 950, 5));
    expect(snap.totalMemory).toBe(16_000);
    expect(snap.sections.length).toBe(4);

    const local = snap.sections[0];
    expect(local.idPrefix).toBe("local");
    expect(local.fixed.length).toBe(2);
    expect(local.fixed[0].label).toBe("Smelt app");
    expect(local.fixed[0].parts.map((p) => p.bytes)).toEqual([200, 180, 120]);
    expect(local.fixed[1].label).toBe("smeltd background service");
    expect(local.sessions.length).toBe(1);
    expect(local.sessions[0].title).toBe("Fix bug");
    expect(local.others?.title).toBe("1 session outside the sidebar");
    expect(local.total).toEqual(usage(18, 950, 5));
    expect(local.note).toBeNull();

    expect(snap.sections[1].note).toBe("timeout");
    expect(snap.sections[2].note).toBe("Offline: connection refused");
    expect(snap.sections[3].note).toBe("Loading…");
  });
});

describe("buildAreaChartSvg", () => {
  test("returns empty string for fewer than 2 points or non-positive max", () => {
    expect(buildAreaChartSvg([])).toBe("");
    expect(buildAreaChartSvg([100])).toBe("");
    expect(buildAreaChartSvg([0, 0, 0])).toBe("");
    expect(buildAreaChartSvg([NaN, NaN])).toBe("");
    expect(buildAreaChartSvg([10, 20], { max: 0 })).toBe("");
  });

  test("generates closed area fill path and open top stroke path with preserveAspectRatio none", () => {
    const svg = buildAreaChartSvg([50, 100, 50], {
      width: 200,
      height: 100,
      max: 100,
      stroke: "#a1a1aa",
      fillOpacity: 0.2,
      strokeOpacity: 0.9,
      strokeWidth: 1.5,
    });
    expect(svg).toContain('viewBox="0 0 200 100"');
    expect(svg).toContain('preserveAspectRatio="none"');
    // Area path closes along baseline y=100
    expect(svg).toContain('d="M0 50 L100 0 L200 50 L200 100 L0 100 Z"');
    expect(svg).toContain('fill="#a1a1aa" fill-opacity="0.2" stroke="none"');
    // Line path strokes only top curve without closing along edges
    expect(svg).toContain('d="M0 50 L100 0 L200 50" fill="none" stroke="#a1a1aa"');
  });

  test("leaves 20% top headroom by default (max = peak * 1.2)", () => {
    const svg = buildAreaChartSvg([0, 120], { width: 100, height: 120 });
    // peak = 120 -> max = 144 -> ratio at 120 = 120/144 = 5/6 -> y = 120 * (1/6) = 20
    expect(svg).toContain('d="M0 120 L100 20"');
  });

  test("splits into separate segments around non-finite gaps", () => {
    const svg = buildAreaChartSvg([10, 20, NaN, 30, 40], {
      width: 400,
      height: 100,
      max: 40,
    });
    // First segment: indices 0,1 (x=0,100); second segment: indices 3,4 (x=300,400)
    expect(svg).toContain('d="M0 75 L100 50 L100 100 L0 100 Z"');
    expect(svg).toContain('d="M300 25 L400 0 L400 100 L300 100 Z"');
  });

  test("normalizes rgba color inputs for SVG 1.1 compatibility", () => {
    const svg = buildAreaChartSvg([10, 20], {
      width: 100,
      height: 50,
      max: 20,
      stroke: "rgba(255, 255, 255, 0.5)",
      fillOpacity: 0.4,
      strokeOpacity: 1,
    });
    expect(svg).toContain('fill="rgb(255,255,255)" fill-opacity="0.2"');
    expect(svg).toContain('stroke="rgb(255,255,255)" stroke-opacity="0.5"');
  });
});
