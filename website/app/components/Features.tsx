import { SectionHeading } from "./SectionHeading";

const FEATURES = [
  {
    kicker: "01",
    title: "告别黑盒终端滚屏，思考、Diff 与文件检查点一目了然",
    body: "不再在几个终端窗口里来回翻找日志。每一次文件修改、终端命令、思考耗时都渲染成清晰可交互的卡片；改坏了随时一键回滚到任意回合检查点，或从半路分叉尝试新方案。",
    span: "lg:col-span-7",
    visual: <ConversationMini />,
  },
  {
    kicker: "02",
    title: "免公网 IP、免配内网穿透，一行命令直连所有 Linux 开发机",
    body: "在远程开发机或 GPU 服务器上跑一条命令，输入配对码即可端到端加密直连。代码、环境与大仓库留在服务器上全速运行，即使合上笔记本重连也毫秒级恢复现场（同时支持自建企业中继）。",
    span: "lg:col-span-5",
    visual: <RelayMini />,
  },
  {
    kicker: "03",
    title: "离开工位不断联：在手机上随时拍板、看进度、回消息",
    body: "智能体遇到操作需要确认？跑到一半向你提问？不用赶回电脑前，掏出手机就能一键批准权限、审阅代码改动、继续交代下一步任务。",
    span: "lg:col-span-5",
    visual: <PassMini />,
  },
  {
    kicker: "04",
    title: "给远程智能体配一块随时可见的「虚拟桌面与浏览器」",
    body: "远程服务器上的智能体不仅能改代码，还能在独立的虚拟桌面和真实 Chrome 里替你跑端到端测试。遇到扫码登录、验证码或样式走样，在 Mac 上点一下就能直接接管画面。",
    span: "lg:col-span-7",
    visual: <DesktopMini />,
  },
  {
    kicker: "05",
    title: "有记忆、能定时值班的常驻智能体团队",
    body: "每个智能体拥有独立的项目记忆（带人工确认防污染）、专属技能与定时 / Webhook 自动触发器，让它们每天定时巡检 CI、清理技术债，或派生子智能体并行拆解复杂任务。",
    span: "lg:col-span-6",
    visual: <AgentMini />,
  },
  {
    kicker: "06",
    title: "集齐你最顺手的 AI 引擎，一套工作台全打通",
    body: "原生内置开箱即用的极速智能体，同时无缝接入 Claude Code、Codex、GitHub Copilot、Cursor、Grok、OpenCode 等主流命令行工具，统一管理各家模型额度与 MCP 扩展能力。",
    span: "lg:col-span-6",
    visual: <EngineMini />,
  },
];

export function Features() {
  return (
    <section id="features" className="mx-auto max-w-6xl scroll-mt-24 px-6 py-24 sm:py-28">
      <SectionHeading
        eyebrow="核心能力"
        title="一台工作台，看住所有机器上的智能体"
        body="思考、改动、远程机器、离开工位之后的审批，和你已经在用的 AI 编程工具，收进同一块屏幕。"
      />
      <div className="mt-12 grid gap-4 lg:grid-cols-12">
        {FEATURES.map((feature) => (
          <article key={feature.kicker} className={`surface-card lift-card flex flex-col p-6 sm:p-7 ${feature.span}`}>
            <p className="font-mono text-[11px] tracking-[0.16em] text-accent">{feature.kicker}</p>
            <h3 className="mt-3 text-xl font-semibold tracking-tight text-foreground">{feature.title}</h3>
            <p className="mt-3 text-sm leading-7 text-muted">{feature.body}</p>
            <div className="mt-6" aria-hidden="true">
              {feature.visual}
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}

function ConversationMini() {
  return (
    <div className="inset-panel overflow-hidden">
      <div className="flex items-center justify-between border-b border-white/[0.06] px-3 py-2 font-mono text-[10px] text-dim">
        <span>思考 · 1.8s · 已折叠</span>
        <span>回合 4</span>
      </div>
      <div className="flex items-center justify-between px-3 py-2 font-mono text-[11px]">
        <span className="text-foreground">src/auth/session.ts</span>
        <span>
          <span className="text-success">+2</span>
          <span className="ml-2 text-danger">−1</span>
        </span>
      </div>
      <div className="font-mono text-[10px] leading-5">
        <p className="bg-[#e07a6a]/10 px-3 text-[#f3c2ba]">42 − const row = db.get(id);</p>
        <p className="bg-[#8fbf9f]/10 px-3 text-[#c9ead6]">42 + const row = await store.session(id);</p>
        <p className="bg-[#8fbf9f]/10 px-3 text-[#c9ead6]">43 + if (!row) throw new SessionMissing(id);</p>
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t border-white/[0.06] px-3 py-2">
        <span className="h-1.5 w-1.5 rounded-full bg-white/25" />
        <span className="h-1.5 w-1.5 rounded-full bg-white/25" />
        <span className="h-1.5 w-1.5 rounded-full bg-accent" />
        <span className="ml-1 text-[10px] text-dim">回合 3</span>
        <span className="ml-auto rounded-full bg-accent px-2 py-0.5 text-[10px] font-semibold text-[#1a1208]">回滚</span>
        <span className="rounded-full border border-white/10 px-2 py-0.5 text-[10px] text-muted">分叉</span>
      </div>
    </div>
  );
}

function RelayMini() {
  return (
    <div className="inset-panel p-3">
      <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-2">
        <Node title="MacBook Pro" meta="原生工作台" />
        <div className="flex flex-col items-center gap-1 px-1">
          <span className="h-px w-8 bg-accent/70" />
          <span className="text-center font-mono text-[9px] leading-3 text-accent">加密直连</span>
          <span className="h-px w-8 bg-accent/70" />
        </div>
        <Node title="Ubuntu GPU" meta="已直连 · 18ms" />
      </div>
      <div className="mt-3 flex items-center justify-between rounded-lg border border-white/[0.06] bg-black/35 px-3 py-2">
        <span className="text-[10px] text-dim">配对码</span>
        <span className="font-mono text-[12px] tracking-[0.18em] text-foreground">K7QX-M2PD</span>
        <span className="text-[10px] text-success">已绑定</span>
      </div>
    </div>
  );
}

function Node({ title, meta }: { title: string; meta: string }) {
  return (
    <div className="rounded-lg border border-white/[0.06] bg-black/30 px-2 py-2">
      <p className="text-[11px] font-medium text-foreground">{title}</p>
      <p className="mt-0.5 text-[10px] text-dim">{meta}</p>
    </div>
  );
}

function PassMini() {
  return (
    <div className="inset-panel p-3">
      <div className="flex items-center justify-between">
        <p className="text-xs font-medium text-foreground">待你拍板 · 2</p>
        <span className="font-mono text-[10px] text-dim">09:41</span>
      </div>
      <div className="mt-2 rounded-lg border border-accent/35 bg-black/30 p-2.5">
        <p className="text-[11px] text-foreground">批准运行数据库迁移</p>
        <p className="mt-1 font-mono text-[10px] text-dim">Ubuntu GPU · pnpm db:migrate</p>
        <div className="mt-2 flex gap-1.5">
          <span className="rounded-md bg-accent px-2 py-0.5 text-[10px] font-semibold text-[#1a1208]">批准运行</span>
          <span className="rounded-md border border-white/10 px-2 py-0.5 text-[10px] text-muted">查看 Diff</span>
        </div>
      </div>
      <div className="mt-2 rounded-lg border border-white/[0.06] bg-black/20 p-2.5">
        <p className="text-[11px] text-foreground">缓存放在哪里？</p>
        <div className="mt-2 flex gap-1.5">
          <span className="rounded-md border border-accent/40 px-2 py-0.5 text-[10px] text-accent">方案 A · Redis</span>
          <span className="rounded-md border border-white/10 px-2 py-0.5 text-[10px] text-muted">方案 B · LRU</span>
        </div>
      </div>
    </div>
  );
}

function DesktopMini() {
  return (
    <div className="inset-panel overflow-hidden">
      <div className="flex items-center gap-2 border-b border-white/[0.06] px-3 py-2">
        <span className="h-2 w-2 rounded-full bg-[#ff5f57]" />
        <span className="h-2 w-2 rounded-full bg-[#febc2e]" />
        <span className="h-2 w-2 rounded-full bg-[#28c840]" />
        <span className="ml-1 flex-1 truncate rounded-md bg-black/40 px-2 py-1 font-mono text-[10px] text-muted">
          https://app.example/login
        </span>
        <span className="rounded-md bg-accent px-2 py-1 text-[10px] font-semibold text-[#1a1208]">点击接管</span>
      </div>
      <div className="relative grid h-36 place-items-center">
        <div className="rounded-xl border border-white/10 bg-white/[0.03] px-6 py-4 text-center">
          <p className="text-xs text-foreground">扫码登录</p>
          <div className="mx-auto mt-2 grid w-12 grid-cols-4 gap-0.5">
            {Array.from({ length: 16 }).map((_, index) => (
              <span key={index} className={`h-2.5 w-2.5 ${index % 3 === 0 ? "bg-white/15" : "bg-foreground/80"}`} />
            ))}
          </div>
        </div>
        <span className="absolute right-10 bottom-6 h-3 w-3 rotate-[-20deg] border-l-[6px] border-b-[8px] border-l-transparent border-b-foreground" />
      </div>
    </div>
  );
}

function AgentMini() {
  return (
    <div className="inset-panel space-y-2 p-3">
      <div className="flex items-center gap-3">
        <span className="grid h-9 w-9 place-items-center rounded-full bg-accent/15 text-xs font-semibold text-accent">巡</span>
        <div>
          <p className="text-sm text-foreground">值班巡检</p>
          <p className="text-[11px] text-dim">每天 09:00 · 看 CI，清掉过期快照</p>
        </div>
      </div>
      <div className="flex items-center justify-between gap-3 rounded-lg bg-white/[0.03] px-3 py-2">
        <span className="text-[11px] text-muted">记忆 · 发布前要先跑迁移</span>
        <span className="shrink-0 rounded-full bg-accent/15 px-2 py-0.5 text-[10px] text-accent">待确认</span>
      </div>
      <Bar label="查失败的 CI" width="100%" />
      <Bar label="清理过期快照" width="68%" />
      <Bar label="子智能体 · 依赖升级" width="36%" />
    </div>
  );
}

function Bar({ label, width }: { label: string; width: string }) {
  return (
    <div>
      <div className="mb-1 flex justify-between text-[10px] text-dim">
        <span>{label}</span>
      </div>
      <div className="h-1.5 overflow-hidden rounded-full bg-white/5">
        <div className="h-full rounded-full bg-accent/80" style={{ width }} />
      </div>
    </div>
  );
}

function EngineMini() {
  const engines = ["内置智能体", "Claude Code", "Codex", "Cursor"];
  const tools = [
    { name: "Playwright", note: "浏览器自动化", on: true },
    { name: "Figma", note: "设计稿", on: true },
    { name: "GitHub", note: "自定义扩展", on: false },
  ];
  return (
    <div className="inset-panel p-3">
      <div className="flex gap-1 overflow-hidden">
        {engines.map((engine, index) => (
          <span
            key={engine}
            className={`rounded-lg px-2 py-1 text-[10px] ${
              index === 0 ? "bg-accent/15 text-accent" : "text-dim"
            }`}
          >
            {engine}
          </span>
        ))}
      </div>
      <ul className="mt-3 space-y-2">
        {tools.map((tool) => (
          <li key={tool.name} className="flex items-center justify-between gap-3">
            <span>
              <span className="block text-xs text-foreground">{tool.name}</span>
              <span className="block text-[10px] text-dim">{tool.note}</span>
            </span>
            <span className={`relative h-5 w-9 rounded-full ${tool.on ? "bg-accent" : "bg-white/10"}`}>
              <span
                className={`absolute top-0.5 h-4 w-4 rounded-full bg-[#1a1208] ${tool.on ? "right-0.5" : "left-0.5"}`}
              />
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
