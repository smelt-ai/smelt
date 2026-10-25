const FEATURES = [
  {
    kicker: "01",
    title: "原生结构化对话，告别黑盒终端滚屏",
    body: "ACP 与单二进制 smelt-pi（JSONL RPC）把思考流、工具调用、内联 Diff、耗时、Plan 和文件检查点画成可点的 GPUI 组件。任意回合可以分叉，也可以回到检查点。",
    span: "lg:col-span-7",
    visual: <ConversationMini />,
  },
  {
    kicker: "02",
    title: "持久化智能体、长期记忆与自动化",
    body: "智能体有自己的工作空间、技能、MCP 和带信任门控的长期记忆。Cron、axum webhook 和进程事件可以在远程机器上把它们叫醒。远程机器上也能直接新建和编辑。",
    span: "lg:col-span-5",
    visual: <AgentMini />,
  },
  {
    kicker: "03",
    title: "子智能体并发与跨会话通信",
    body: "主智能体用 subagent 把子任务并行派出去，右侧抽屉里审轨迹。session_list、session_read、session_send、session_spawn 让多段会话互相发消息。",
    span: "lg:col-span-5",
    visual: <SubagentMini />,
  },
  {
    kicker: "04",
    title: "iroh P2P，零公网 IP",
    body: "smeltd 和界面拆开。smeltd pair 打出一次性配对码，iroh QUIC 端到端加密直连 Linux。自建中继用 smeltd relay，或安装时 --relay。",
    span: "lg:col-span-7",
    visual: <RelayMini />,
  },
  {
    kicker: "05",
    title: "内嵌 Linux 远程桌面",
    body: "install.sh --with-desktop 装上 TigerVNC、XFCE 和常驻 Chrome（CDP 9222），并执行 smeltd mcp add playwright。Mac 上点「桌面」就能看，也能接管。",
    span: "lg:col-span-6",
    visual: <DesktopMini />,
  },
  {
    kicker: "06",
    title: "手机与平板「出菜口」",
    body: "Flutter 客户端同时连上所有已配对机器。首屏是待审批、待回答和各机器进度。四栏是指挥台、会话、智能体、设置；平板三栏；中英双语。",
    span: "lg:col-span-6",
    visual: <PassMini />,
  },
];

export function Features() {
  return (
    <section id="features" className="mx-auto max-w-6xl scroll-mt-20 px-6 py-20">
      <p className="font-mono text-[11px] tracking-[0.18em] text-accent">核心能力</p>
      <h2 className="mt-3 max-w-2xl text-3xl font-semibold tracking-tight text-foreground">
        六件别的外壳还没同时做完的事
      </h2>
      <p className="mt-3 max-w-2xl text-sm leading-6 text-muted">
        结构化对话、常驻智能体、子任务并行、无公网 IP 的跨机器连接、远程桌面里的浏览器，以及离开工位之后的拍板。
      </p>

      <div className="mt-10 grid gap-3 lg:grid-cols-12">
        {FEATURES.map((feature) => (
          <article
            key={feature.kicker}
            className={`flex flex-col justify-between rounded-lg border border-border bg-surface p-5 ${feature.span}`}
          >
            <div>
              <p className="font-mono text-[11px] text-accent">{feature.kicker}</p>
              <h3 className="mt-2 text-lg font-semibold tracking-tight text-foreground">
                {feature.title}
              </h3>
              <p className="mt-2 text-sm leading-6 text-muted">{feature.body}</p>
            </div>
            <div className="mt-5">{feature.visual}</div>
          </article>
        ))}
      </div>
    </section>
  );
}

function ConversationMini() {
  return (
    <div className="grid gap-2 font-mono text-[11px] sm:grid-cols-3">
      <Mini label="思考" value="折叠 · 1.8s" />
      <Mini label="Diff" value="install.sh +18" />
      <Mini label="检查点" value="回合 14 · 可回滚" />
    </div>
  );
}

function AgentMini() {
  return (
    <div className="rounded-md border border-border bg-background px-3 py-2 font-mono text-[11px] leading-5 text-muted">
      <p>space · ~/.smelt/agents/&lt;id&gt;</p>
      <p>memory · pending → approve</p>
      <p className="text-accent">cron 09:00 · webhook 127.0.0.1</p>
    </div>
  );
}

function SubagentMini() {
  return (
    <div className="flex flex-wrap gap-2 font-mono text-[11px]">
      {["session_list", "session_read", "session_send", "session_spawn"].map((tool) => (
        <span key={tool} className="rounded-md border border-border bg-background px-2 py-1 text-muted">
          {tool}
        </span>
      ))}
    </div>
  );
}

function RelayMini() {
  return (
    <div className="rounded-md border border-border bg-background px-3 py-2 font-mono text-[11px] leading-6 text-muted">
      <p>smeltd pair</p>
      <p>smeltd relay https://relay.example.com</p>
      <p className="text-dim">smeltd relay off · 回到 n0 公共中继</p>
    </div>
  );
}

function DesktopMini() {
  return (
    <div className="flex flex-wrap gap-2 font-mono text-[11px] text-muted">
      {["smelt-display", "smelt-desktop", "smelt-chrome", "CDP :9222"].map((item) => (
        <span key={item} className="rounded-md border border-border bg-background px-2 py-1">
          {item}
        </span>
      ))}
    </div>
  );
}

function PassMini() {
  return (
    <div className="grid grid-cols-4 overflow-hidden rounded-md border border-border font-mono text-[11px]">
      <span className="bg-surface-3 py-2 text-center text-accent">指挥台</span>
      <span className="py-2 text-center text-dim">会话</span>
      <span className="py-2 text-center text-dim">智能体</span>
      <span className="py-2 text-center text-dim">设置</span>
    </div>
  );
}

function Mini({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border border-border bg-background px-3 py-2">
      <p className="text-dim">{label}</p>
      <p className="mt-1 text-foreground">{value}</p>
    </div>
  );
}
