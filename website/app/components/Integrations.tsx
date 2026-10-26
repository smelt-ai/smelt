import { SectionHeading } from "./SectionHeading";

const AGENTS = [
  {
    name: "内置智能体",
    tag: "随桌面附带",
    note: "开箱即用，完整支持文件检查点、子任务并发与项目记忆。不用再单独准备一套运行环境。",
    wide: true,
  },
  {
    name: "Claude Code",
    tag: "已安装即可接入",
    note: "复用你现有的订阅与配置，升级为可视化卡片、Diff 审阅与手机远程管控。",
  },
  {
    name: "Codex",
    tag: "已安装即可接入",
    note: "和 Claude Code 并排跑在同一套审批与 Diff 里，方便两条方案对照着看。",
  },
  {
    name: "GitHub Copilot",
    tag: "已安装即可接入",
    note: "沿用已有登录，在工作台里确认命令、审阅改动，不用在终端滚屏里翻结果。",
  },
  {
    name: "Cursor",
    tag: "已安装即可接入",
    note: "把 Cursor 的命令行智能体放进同一块屏幕。会话留在它正在跑的那台机器上。",
  },
  {
    name: "Grok",
    tag: "已安装即可接入",
    note: "同样的卡片式对话、权限审批和跨机器会话，离开电脑也能在手机上接着看。",
  },
  {
    name: "OpenCode",
    tag: "已安装即可接入",
    note: "复用你已经配好的模型与密钥，改动和审批跟其它引擎走同一套界面。",
  },
  {
    name: "Kiro",
    tag: "已安装即可接入",
    note: "接入后用同样的工具卡片和权限确认，人在外面也能用手机继续拍板。",
  },
  {
    name: "Crush",
    tag: "已安装即可接入",
    note: "终端里顺手的引擎，在这里可以看清 Diff、回滚到某一回合，再用手机接着指挥。",
  },
];

const MCP = [
  {
    name: "Playwright",
    tag: "浏览器自动化",
    note: "在远程桌面里的 Chrome 中打开页面、点击、截图，跑端到端验证。",
    on: true,
  },
  {
    name: "Figma",
    tag: "设计稿读取",
    note: "读取画板、组件和变量，按设计稿生成代码。用 Figma 账号授权一次即可。",
    on: true,
  },
  {
    name: "REA",
    tag: "逆向分析",
    note: "拆解应用、二进制和网页，带证据说明一个功能是怎么实现的。",
    on: false,
  },
  {
    name: "浏览器",
    tag: "已登录的 Chrome",
    note: "操作你已经登录的浏览器。遇到验证码会停下来问你。",
    on: false,
  },
];

const MARKS = ["Rust + GPUI 原生 120fps 渲染", "端到端加密直连", "断线不丢会话", "Apache-2.0 开源"];

export function Integrations() {
  return (
    <section id="agents" className="scroll-mt-24 border-y border-white/[0.06] bg-white/[0.015]">
      <div className="mx-auto max-w-6xl px-6 py-24 sm:py-28">
        <SectionHeading
          eyebrow="智能体生态"
          title="你顺手的 AI 编程引擎，放进同一套工作台"
          body="内置智能体开箱即用。其余引擎复用现有订阅和配置，升级成可视化卡片、Diff 审阅和手机远程管控。"
        />

        <div className="mt-12 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {AGENTS.map((agent) => (
            <article
              key={agent.name}
              className={`surface-card p-5 ${agent.wide ? "sm:col-span-2 lg:col-span-4" : ""}`}
            >
              <div className="flex items-baseline justify-between gap-3">
                <h3 className="text-sm font-semibold text-foreground">{agent.name}</h3>
                <span className="shrink-0 text-[11px] text-accent">{agent.tag}</span>
              </div>
              <p className="mt-2 text-sm leading-6 text-muted">{agent.note}</p>
              {agent.wide ? (
                <ul className="mt-4 flex flex-wrap gap-2">
                  {["文件检查点", "子任务并发", "项目记忆", "专属技能"].map((item) => (
                    <li key={item} className="rounded-full border border-white/10 px-2.5 py-1 text-[11px] text-muted">
                      {item}
                    </li>
                  ))}
                </ul>
              ) : null}
            </article>
          ))}
        </div>

        <div className="mt-14 grid gap-8 lg:grid-cols-[minmax(0,1.15fr)_minmax(0,0.85fr)]">
          <div>
            <h3 className="text-lg font-semibold tracking-tight text-foreground">开箱即用的 MCP 扩展</h3>
            <p className="mt-2 max-w-xl text-sm leading-7 text-muted">
              浏览器自动化、设计稿读取、逆向分析，以及代码仓库、网页检索等你自己添加的扩展。可以在界面里一键启用，也可以执行{" "}
              <code className="font-mono text-[13px] text-accent">smeltd mcp add</code>。
            </p>
            <ul className="mt-5 space-y-2">
              {MCP.map((item) => (
                <li
                  key={item.name}
                  className="flex items-center justify-between gap-4 rounded-2xl border border-white/[0.08] bg-black/20 px-4 py-3"
                >
                  <div>
                    <p className="text-sm font-medium text-foreground">
                      {item.name}
                      <span className="ml-2 text-[11px] font-normal text-dim">{item.tag}</span>
                    </p>
                    <p className="mt-1 text-xs leading-5 text-muted">{item.note}</p>
                  </div>
                  <span
                    aria-hidden="true"
                    className={`relative h-6 w-11 shrink-0 rounded-full ${item.on ? "bg-accent" : "bg-white/10"}`}
                  >
                    <span
                      className={`absolute top-0.5 h-5 w-5 rounded-full bg-[#1a1208] ${item.on ? "right-0.5" : "left-0.5"}`}
                    />
                  </span>
                </li>
              ))}
            </ul>
            <p className="mt-4 font-mono text-xs text-dim">
              <span className="text-muted">$ </span>smeltd mcp add playwright
            </p>
          </div>
          <div className="surface-card h-fit p-5">
            <h3 className="text-sm font-semibold text-foreground">底层品质</h3>
            <ul className="mt-4 space-y-2">
              {MARKS.map((item) => (
                <li key={item} className="rounded-xl border border-white/[0.06] bg-black/25 px-3 py-2.5 text-sm text-muted">
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
