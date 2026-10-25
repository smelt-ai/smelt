const AGENTS = [
  {
    name: "Pi",
    mark: "smelt-pi",
    note: "一等公民。单二进制跑官方 JSONL RPC：文件检查点、子智能体、跨会话通信、长期记忆和技能都在这条运行时上。",
    first: true,
  },
  {
    name: "Claude Code",
    mark: "ACP",
    note: "受管运行时接入。结构化工具卡片、权限审批、跨机器会话持久化。",
  },
  {
    name: "Codex",
    mark: "ACP",
    note: "同一套对话壳。工具卡、审批和会话都落到那台机器的 smeltd。",
  },
  {
    name: "GitHub Copilot",
    mark: "ACP",
    note: "和其它 ACP 智能体共用权限门与跨机器目录。",
  },
  {
    name: "Cursor",
    mark: "ACP",
    note: "会话留在运行它的那台机器上，桌面和手机只是客户端。",
  },
  {
    name: "Grok",
    mark: "ACP",
    note: "结构化消息流，不把终端滚屏当作唯一界面。",
  },
  {
    name: "OpenCode",
    mark: "ACP",
    note: "经 ACP 适配器接入，权限和恢复走同一条控制面。",
  },
  {
    name: "Kiro",
    mark: "ACP",
    note: "受管运行时拉起。工具卡片和审批与其它引擎一致。",
  },
  {
    name: "Crush",
    mark: "ACP",
    note: "终端里能跑的，也可以并排看住；结构化接入走 ACP。",
  },
];

const MCP = [
  {
    id: "playwright",
    note: "有远程桌面就连 CDP :9222，否则 --extension",
  },
  { id: "figma", note: "内置预设，smeltd mcp add figma" },
  { id: "rea", note: "内置预设" },
  { id: "browser-mcp", note: "浏览器扩展模式" },
];

const STACK = [
  "Rust 2024",
  "GPUI",
  "smeltd · Control API v3",
  "iroh QUIC",
  "SQLite WAL · smelt.sqlite3 v24",
  "client.sqlite3 v4",
  "Flutter + Rust FFI",
];

export function Integrations() {
  return (
    <section id="agents" className="scroll-mt-20 border-y border-border bg-surface/40">
      <div className="mx-auto max-w-6xl px-6 py-20">
        <p className="font-mono text-[11px] tracking-[0.18em] text-accent">智能体生态</p>
        <h2 className="mt-3 max-w-2xl text-3xl font-semibold tracking-tight text-foreground">
          九个原生编程智能体，加上一套 MCP
        </h2>
        <p className="mt-3 max-w-2xl text-sm leading-6 text-muted">
          Pi 编译成 ~/.smelt/bin/smelt-pi。其余引擎经 ACP 和受管 Bun（~/.smelt/runtime）接入，共用工具卡片、审批和跨机器持久化。
        </p>

        <div className="mt-10 grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {AGENTS.map((agent) => (
            <article
              key={agent.name}
              className={`rounded-lg border p-4 ${
                agent.first
                  ? "border-accent/50 bg-background sm:col-span-2 lg:col-span-3"
                  : "border-border bg-background"
              }`}
            >
              <div className="flex items-baseline justify-between gap-3">
                <h3 className="text-sm font-semibold text-foreground">{agent.name}</h3>
                <span className="font-mono text-[10px] text-accent">{agent.mark}</span>
              </div>
              <p className="mt-2 text-sm leading-6 text-muted">{agent.note}</p>
            </article>
          ))}
        </div>

        <div className="mt-12 grid gap-8 lg:grid-cols-[minmax(0,1.2fr)_minmax(0,0.8fr)]">
          <div>
            <h3 className="text-sm font-semibold text-foreground">内置 MCP 预设</h3>
            <p className="mt-2 text-sm leading-6 text-muted">
              smeltd mcp add &lt;id&gt; 写入 Pi 的 mcp.json。已有同名服务器就不动。自定义 stdio / HTTP 仍可在能力库里加。
            </p>
            <ul className="mt-4 divide-y divide-border border-y border-border">
              {MCP.map((item) => (
                <li key={item.id} className="flex flex-col gap-1 py-3 sm:flex-row sm:items-baseline sm:justify-between">
                  <span className="font-mono text-sm text-foreground">{item.id}</span>
                  <span className="text-sm text-muted">{item.note}</span>
                </li>
              ))}
            </ul>
          </div>
          <div>
            <h3 className="text-sm font-semibold text-foreground">底层</h3>
            <ul className="mt-4 space-y-2">
              {STACK.map((item) => (
                <li
                  key={item}
                  className="rounded-md border border-border bg-background px-3 py-2 font-mono text-[12px] text-muted"
                >
                  {item}
                </li>
              ))}
            </ul>
          </div>
        </div>
      </div>
    </section>
  );
}
