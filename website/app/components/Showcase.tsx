import { SectionHeading } from "./SectionHeading";

const STEPS = [
  {
    n: "01",
    title: "打开原生工作台",
    body: "在 Mac 上安装 Smelt 桌面端，开箱即可在本机项目里启动内置智能体或接入已安装的 Claude Code / Codex。",
  },
  {
    n: "02",
    title: "一行命令配对远程服务器",
    body: "在 Linux 开发机或 GPU 服务器上运行一键安装脚本，执行 smeltd pair 得到一次性配对码，在桌面端或手机端粘贴即可完成加密绑定。",
  },
  {
    n: "03",
    title: "桌面深入编码，手机随身拍板",
    body: "所有机器的会话、远程桌面与定时任务实时同步；在工位用大屏分屏调度，离开工位用手机随时审批与指挥。",
  },
];

const NODES = [
  {
    mark: "Mac",
    title: "本机 Mac",
    kicker: "120fps 原生桌面体验",
    points: ["并行指挥多个智能体", "大屏看 Diff 和远程桌面", "工位上的主操作台"],
  },
  {
    mark: "Lin",
    title: "远程 Linux 服务器",
    kicker: "代码与环境留在远端",
    points: ["端到端加密直连", "仓库和工具链不用搬回来", "合上笔记本，会话还在"],
  },
  {
    mark: "机",
    title: "手机随身端",
    kicker: "随身审批与进度掌控",
    points: ["批准命令、查看 Diff", "回答智能体的提问", "通勤路上继续推进"],
  },
];

export function Showcase() {
  return (
    <section id="guide" className="mx-auto max-w-6xl scroll-mt-24 px-6 py-24 sm:py-28">
      <SectionHeading
        eyebrow="上手指南"
        title="三步连上你的全部开发机"
        body="从一台 Mac 开始，把实验室和云端的 Linux 服务器接进来，再用手机补上离开工位之后的时间。"
      />

      <ol className="mt-12 grid gap-4 lg:grid-cols-3">
        {STEPS.map((step) => (
          <li key={step.n} className="surface-card flex flex-col p-6">
            <p className="font-mono text-xs tracking-[0.16em] text-accent">Step {step.n}</p>
            <h3 className="mt-3 text-lg font-semibold tracking-tight text-foreground">{step.title}</h3>
            <p className="mt-2 text-sm leading-7 text-muted">{step.body}</p>
            <div className="mt-5">{step.n === "01" ? <MacStep /> : step.n === "02" ? <PairStep /> : <SyncStep />}</div>
          </li>
        ))}
      </ol>

      <div className="mt-16">
        <h3 className="text-2xl font-semibold tracking-tight text-foreground">本机 Mac、远程 Linux，和口袋里的手机</h3>
        <p className="mt-3 max-w-2xl text-sm leading-7 text-muted">
          三端看的是同一批会话。桌面负责深入编码，服务器留着代码和环境，手机负责你不在键盘前的那些决定。
        </p>
        <div className="relative mt-10">
          <div
            aria-hidden="true"
            className="pointer-events-none absolute top-5 right-[12%] left-[12%] hidden h-px bg-gradient-to-r from-transparent via-accent/70 to-transparent lg:block"
          />
          <div className="grid gap-4 lg:grid-cols-3">
            {NODES.map((node) => (
              <article key={node.title} className="surface-card p-5">
                <div className="relative z-10 mb-4 grid h-10 w-10 place-items-center rounded-full border border-accent/40 bg-[#101113] text-[11px] font-semibold text-accent">
                  {node.mark}
                </div>
                <h4 className="text-base font-semibold text-foreground">{node.title}</h4>
                <p className="mt-1 text-sm text-accent">{node.kicker}</p>
                <ul className="mt-4 space-y-2 text-sm text-muted">
                  {node.points.map((point) => (
                    <li key={point} className="flex gap-2">
                      <span className="mt-2 h-1 w-1 shrink-0 rounded-full bg-accent" />
                      <span>{point}</span>
                    </li>
                  ))}
                </ul>
              </article>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}

function MacStep() {
  return (
    <div className="inset-panel overflow-hidden" aria-hidden="true">
      <div className="flex items-center gap-1.5 border-b border-white/[0.06] px-3 py-2">
        <span className="h-2 w-2 rounded-full bg-[#ff5f57]" />
        <span className="h-2 w-2 rounded-full bg-[#febc2e]" />
        <span className="h-2 w-2 rounded-full bg-[#28c840]" />
        <span className="ml-2 text-[10px] text-dim">本机项目</span>
      </div>
      <div className="space-y-1.5 p-3 text-[11px]">
        <p className="text-foreground">内置智能体 · 已就绪</p>
        <p className="text-muted">Claude Code · 已接入</p>
        <p className="text-muted">Codex · 已接入</p>
      </div>
    </div>
  );
}

function PairStep() {
  return (
    <div className="inset-panel p-3 font-mono text-[10px] leading-5" aria-hidden="true">
      <p className="text-dim">$ smeltd pair</p>
      <p className="mt-2 text-muted">一次性配对码</p>
      <p className="tracking-[0.18em] text-foreground">K7QX-M2PD</p>
      <p className="mt-2 text-dim">十分钟内有效 · 用一次</p>
    </div>
  );
}

function SyncStep() {
  return (
    <div className="inset-panel grid grid-cols-2 gap-2 p-3" aria-hidden="true">
      <div className="rounded-lg border border-white/[0.06] px-2 py-2">
        <p className="text-[10px] text-dim">桌面</p>
        <p className="mt-1 text-[11px] text-foreground">分屏调度</p>
      </div>
      <div className="rounded-lg border border-accent/30 px-2 py-2">
        <p className="text-[10px] text-accent">手机</p>
        <p className="mt-1 text-[11px] text-foreground">随身拍板</p>
      </div>
    </div>
  );
}
