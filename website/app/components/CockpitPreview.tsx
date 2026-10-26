"use client";

import { useState, type KeyboardEvent, type ReactNode } from "react";

const TABS = [
  {
    id: "desktop",
    label: "桌面多机工作台",
    hint: "本机桌面与远程 Linux 同屏指挥",
  },
  {
    id: "mobile",
    label: "手机随身审批与指挥",
    hint: "离开工位，批准、看 Diff、接着说",
  },
  {
    id: "remote",
    label: "内嵌远程桌面与浏览器",
    hint: "扫码登录和界面验证，点一下接管",
  },
  {
    id: "fork",
    label: "回合检查点与方案分叉",
    hint: "改乱了就回退，或分叉两条方案",
  },
] as const;

type TabId = (typeof TABS)[number]["id"];
type Decision = "pending" | "approved" | "denied";

export function CockpitPreview() {
  const [tab, setTab] = useState<TabId>("desktop");

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const index = TABS.findIndex((item) => item.id === tab);
    if (event.key === "ArrowRight") {
      event.preventDefault();
      setTab(TABS[(index + 1) % TABS.length].id);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      setTab(TABS[(index - 1 + TABS.length) % TABS.length].id);
    }
  }

  return (
    <section id="preview" aria-label="产品预览" className="mt-14 scroll-mt-24">
      <div
        role="tablist"
        aria-label="使用场景"
        onKeyDown={onKeyDown}
        className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4"
      >
        {TABS.map((item) => {
          const active = tab === item.id;
          return (
            <button
              key={item.id}
              id={`tab-${item.id}`}
              type="button"
              role="tab"
              aria-selected={active}
              aria-controls={`panel-${item.id}`}
              tabIndex={active ? 0 : -1}
              onClick={() => setTab(item.id)}
              className={`rounded-2xl border px-3.5 py-3 text-left transition ${
                active
                  ? "border-accent/50 bg-accent/[0.08] shadow-[inset_0_1px_0_rgba(255,255,255,0.08)]"
                  : "border-white/[0.08] bg-white/[0.02] hover:border-white/15 hover:bg-white/[0.04]"
              }`}
            >
              <span className={`flex items-center gap-2 text-sm font-medium ${active ? "text-accent" : "text-foreground"}`}>
                <TabIcon id={item.id} />
                {item.label}
              </span>
              <span className="mt-1 block text-xs leading-5 text-muted">{item.hint}</span>
            </button>
          );
        })}
      </div>

      <div
        key={tab}
        role="tabpanel"
        id={`panel-${tab}`}
        aria-labelledby={`tab-${tab}`}
        className="scene-enter mt-4"
      >
        {tab === "desktop" ? <DesktopView /> : null}
        {tab === "mobile" ? <MobileView /> : null}
        {tab === "remote" ? <RemoteView /> : null}
        {tab === "fork" ? <ForkView /> : null}
      </div>
    </section>
  );
}

function DesktopView() {
  const [decision, setDecision] = useState<Decision>("pending");
  const [showDiff, setShowDiff] = useState(false);
  const [thinkingOpen, setThinkingOpen] = useState(false);
  const [checkpoint, setCheckpoint] = useState<null | "rollback" | "fork">(null);

  return (
    <div className="relative">
      <div className="surface-card overflow-hidden">
        <WindowChrome title="Smelt" subtitle="MacBook Pro" trailing="120 fps" />
        <div className="grid min-h-[540px] lg:grid-cols-[228px_minmax(0,1fr)]">
          <aside className="border-b border-white/[0.06] p-3 lg:border-r lg:border-b-0">
            <p className="px-2 font-mono text-[10px] tracking-[0.16em] text-dim">机器</p>
            <Machine
              name="本机 MacBook Pro"
              meta="本地"
              sessions={[
                { title: "登录态迁移", agent: "Claude Code", state: "正在修改 3 个文件", tone: "run", active: true },
                { title: "官网文案", agent: "内置智能体", state: "空闲", tone: "idle" },
              ]}
            />
            <Machine
              name="Ubuntu GPU 开发机"
              meta="已直连 · 18ms"
              sessions={[{ title: "训练脚本", agent: "Codex", state: "等待命令确认", tone: "wait" }]}
            />
          </aside>

          <div className="p-4 lg:pr-4">
            <div className="flex items-center justify-between gap-3">
              <div>
                <p className="text-sm font-medium text-foreground">登录态迁移</p>
                <p className="mt-0.5 text-[11px] text-dim">Claude Code · 本机 MacBook Pro</p>
              </div>
              <span className="rounded-full border border-white/10 px-2 py-1 font-mono text-[10px] text-muted">
                回合 4
              </span>
            </div>

            <button
              type="button"
              onClick={() => setThinkingOpen((open) => !open)}
              aria-expanded={thinkingOpen}
              className="mt-4 flex w-full items-center justify-between rounded-xl border border-white/[0.06] bg-black/25 px-3 py-2 text-left"
            >
              <span className="font-mono text-[11px] text-muted">{thinkingOpen ? "▾" : "▸"} 思考 · 已折叠</span>
              <span className="font-mono text-[11px] text-dim">1.8s</span>
            </button>
            {thinkingOpen ? (
              <p className="mt-2 rounded-xl border border-white/[0.04] bg-black/20 px-3 py-2 text-xs leading-6 text-muted">
                迁移要先读现有会话，再决定是补字段还是新开一张表。先改 loadSession，缺记录时给出明确错误，而不是返回空对象。
              </p>
            ) : null}

            <div className="mt-3 overflow-hidden rounded-xl border border-white/[0.06] bg-black/30">
              <div className="flex items-center justify-between gap-3 border-b border-white/[0.06] px-3 py-2">
                <span className="font-mono text-[11px] text-foreground">src/auth/session.ts</span>
                <span className="font-mono text-[11px]">
                  <span className="text-success">+2</span>
                  <span className="ml-2 text-danger">−1</span>
                </span>
              </div>
              <DiffLines />
              <div className="flex flex-wrap gap-2 border-t border-white/[0.06] px-3 py-2">
                <button type="button" onClick={() => setCheckpoint("rollback")} className={checkpoint === "rollback" ? miniOn : miniOff}>
                  回滚到此回合检查点
                </button>
                <button type="button" onClick={() => setCheckpoint("fork")} className={checkpoint === "fork" ? miniOn : miniOff}>
                  从此处分叉
                </button>
              </div>
            </div>
            {checkpoint ? (
              <p className="mt-2 text-[11px] text-accent" aria-live="polite">
                {checkpoint === "rollback" ? "已回到这一回合的文件快照。" : "将从这里分叉出一条新会话。"}
              </p>
            ) : null}

            <div className="mt-3 rounded-xl border border-accent/30 bg-black/30 p-3 lg:mr-[248px]">
              <div className="flex items-center justify-between gap-3">
                <p className="font-mono text-[11px] text-accent">终端 · 等待命令确认</p>
                <DecisionLabel decision={decision} />
              </div>
              <p className="mt-2 font-mono text-[12.5px] text-foreground">
                <span className="text-dim">$ </span>pnpm db:migrate
              </p>
              {decision === "pending" ? (
                <div className="mt-3 flex gap-2">
                  <button type="button" onClick={() => setDecision("approved")} className={miniOn}>
                    批准执行
                  </button>
                  <button type="button" onClick={() => setDecision("denied")} className={miniOff}>
                    拒绝
                  </button>
                </div>
              ) : null}
            </div>
          </div>
        </div>
      </div>

      <div className="relative z-10 mt-4 lg:absolute lg:right-5 lg:bottom-5 lg:mt-0 lg:w-[236px]">
        <div className="float-phone">
          <PhoneFrame>
            <div className="px-3 pb-3">
              <p className="font-mono text-[10px] tracking-[0.14em] text-accent">高危命令审批</p>
              <p className="mt-2 text-[13px] font-medium leading-5 text-foreground">数据库迁移还没执行</p>
              <p className="mt-1 text-[11px] leading-5 text-muted">登录态迁移 · 本机 MacBook Pro</p>
              <p className="mt-2 rounded-lg bg-black/40 px-2 py-1.5 font-mono text-[10px] text-foreground">
                pnpm db:migrate
              </p>
              {showDiff ? (
                <p className="mt-2 rounded-lg bg-[#8fbf9f]/10 px-2 py-1 font-mono text-[10px] text-[#c9ead6]">
                  + create table session_grant
                </p>
              ) : null}
              <DecisionLabel decision={decision} />
              {decision === "pending" ? (
                <div className="mt-3 grid grid-cols-2 gap-1.5">
                  <button type="button" onClick={() => setDecision("approved")} className={miniOn}>
                    批准执行
                  </button>
                  <button type="button" onClick={() => setShowDiff((open) => !open)} className={miniOff}>
                    查看改动
                  </button>
                </div>
              ) : (
                <p className="mt-3 text-[11px] leading-5 text-muted">桌面端会同步这次决定，不用回到键盘前。</p>
              )}
            </div>
          </PhoneFrame>
        </div>
      </div>
    </div>
  );
}

function MobileView() {
  const [migration, setMigration] = useState<Decision>("pending");
  const [choice, setChoice] = useState<null | "redis" | "lru">(null);
  const [showDiff, setShowDiff] = useState(false);

  return (
    <div className="surface-card overflow-hidden">
      <WindowChrome title="离开工位" subtitle="咖啡厅 · 通勤路上" trailing="2 件待拍板" />
      <div className="grid gap-6 p-4 sm:p-6 lg:grid-cols-[260px_minmax(0,1fr)] lg:items-start">
        <PhoneFrame caption="iPhone">
          <div className="px-3 pb-4">
            <p className="text-[13px] font-medium text-foreground">待你拍板</p>
            <p className="mt-0.5 text-[11px] text-dim">两台开发机上的事，汇总在这一屏</p>
            <article className="mt-3 rounded-xl border border-accent/35 bg-black/30 p-2.5">
              <p className="text-xs font-medium text-foreground">批准执行数据库迁移</p>
              <p className="mt-1 font-mono text-[10px] text-dim">Ubuntu GPU · pnpm db:migrate</p>
              {migration === "pending" ? (
                <div className="mt-2 flex gap-1.5">
                  <button type="button" onClick={() => setMigration("approved")} className={miniOn}>
                    批准执行
                  </button>
                  <button type="button" onClick={() => setShowDiff((open) => !open)} className={miniOff}>
                    查看 Diff
                  </button>
                </div>
              ) : (
                <p className="mt-2 text-[11px] text-success">已批准，智能体继续执行</p>
              )}
              {showDiff ? (
                <p className="mt-2 rounded-lg bg-[#8fbf9f]/10 px-2 py-1 font-mono text-[10px] text-[#c9ead6]">
                  + create table session_grant
                </p>
              ) : null}
            </article>
            <article className="mt-2 rounded-xl border border-white/[0.06] bg-black/25 p-2.5">
              <p className="text-xs font-medium text-foreground">缓存放在哪里？</p>
              <p className="mt-1 text-[11px] leading-5 text-muted">Redis，还是进程内 LRU？</p>
              <div className="mt-2 flex gap-1.5">
                <button type="button" onClick={() => setChoice("redis")} className={choice === "redis" ? miniOn : miniOff}>
                  方案 A
                </button>
                <button type="button" onClick={() => setChoice("lru")} className={choice === "lru" ? miniOn : miniOff}>
                  方案 B
                </button>
              </div>
              {choice ? (
                <p className="mt-2 text-[11px] text-success">
                  已回复 · {choice === "redis" ? "用 Redis" : "用进程内 LRU"}
                </p>
              ) : (
                <p className="mt-2 text-[10px] text-dim">也可以直接语音或文字回复</p>
              )}
            </article>
          </div>
        </PhoneFrame>

        <div>
          <p className="mb-3 text-xs text-dim">iPad · 导航、待办、详情</p>
          <div className="overflow-hidden rounded-[24px] border border-white/10 bg-[#0c0d0f] shadow-[0_24px_70px_-36px_rgba(0,0,0,0.85)]">
            <div className="grid sm:grid-cols-[76px_168px_minmax(0,1fr)]">
              <div className="hidden flex-col gap-2 border-white/[0.06] p-3 sm:flex sm:border-r">
                {["待办", "会话", "智能体"].map((item, index) => (
                  <span
                    key={item}
                    className={`rounded-lg px-2 py-2 text-center text-[11px] ${index === 0 ? "bg-white/[0.06] text-accent" : "text-dim"}`}
                  >
                    {item}
                  </span>
                ))}
              </div>
              <div className="border-b border-white/[0.06] p-3 sm:border-r sm:border-b-0">
                <p className="font-mono text-[10px] tracking-[0.14em] text-dim">全部机器</p>
                <p className="mt-3 text-xs text-foreground">数据库迁移</p>
                <p className="text-[10px] text-dim">Ubuntu GPU</p>
                <p className="mt-3 text-xs text-foreground">缓存选型</p>
                <p className="text-[10px] text-dim">MacBook Pro</p>
              </div>
              <div className="p-4">
                <p className="text-sm font-medium text-foreground">批准执行数据库迁移</p>
                <p className="mt-1 text-xs leading-5 text-muted">
                  智能体准备在 Ubuntu GPU 开发机上执行迁移。批准后它会继续，拒绝则停在这一步等你改口。
                </p>
                <div className="mt-3 overflow-hidden rounded-lg border border-white/[0.06]">
                  <div className="flex justify-between border-b border-white/[0.06] px-2 py-1 font-mono text-[10px]">
                    <span>db/migrations/004_session.sql</span>
                    <span className="text-success">+18</span>
                  </div>
                  <p className="bg-[#8fbf9f]/10 px-2 py-1 font-mono text-[10px] text-[#b7dcc4]">+ create table session_grant</p>
                </div>
                <div className="mt-3">
                  {migration === "pending" ? (
                    <button type="button" onClick={() => setMigration("approved")} className={miniOn}>
                      批准执行
                    </button>
                  ) : (
                    <p className="text-xs text-success">已批准执行</p>
                  )}
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function RemoteView() {
  const [taken, setTaken] = useState(false);

  return (
    <div className="surface-card overflow-hidden">
      <WindowChrome title="远程桌面" subtitle="Ubuntu GPU 开发机" trailing={taken ? "你正在接管" : "只看"} />
      <div className="grid min-h-[460px] lg:grid-cols-2">
        <div className="border-b border-white/[0.06] p-4 lg:border-r lg:border-b-0">
          <p className="text-sm font-medium text-foreground">内置智能体 · 端到端验证</p>
          <p className="mt-1 text-xs leading-5 text-muted">它先自己把前端跑起来，再打开浏览器走登录。卡在扫码时把画面交给你。</p>
          <ol className="mt-4 space-y-2">
            {[
              ["01", "启动前端服务", false],
              ["02", "打开登录页", false],
              ["03", "填入测试账号", false],
              ["04", "停在扫码登录，等待接管", true],
            ].map(([step, label, current]) => (
              <li
                key={step as string}
                className={`flex items-center gap-3 rounded-xl border px-3 py-2.5 ${
                  current ? "border-accent/40 bg-accent/[0.06]" : "border-white/[0.06] bg-black/20"
                }`}
              >
                <span className={`font-mono text-[11px] ${current ? "text-accent" : "text-dim"}`}>{step}</span>
                <span className="text-sm text-foreground">{label}</span>
                {current ? <span className="pulse-dot ml-auto h-1.5 w-1.5 rounded-full bg-accent" /> : null}
              </li>
            ))}
          </ol>
        </div>
        <div className="bg-black/20 p-4">
          <div className={`overflow-hidden rounded-xl border ${taken ? "border-accent/60" : "border-white/10"}`}>
            <div className="flex items-center gap-2 border-b border-white/[0.06] bg-white/[0.03] px-3 py-2">
              <span className="h-2 w-2 rounded-full bg-[#ff5f57]" />
              <span className="h-2 w-2 rounded-full bg-[#febc2e]" />
              <span className="h-2 w-2 rounded-full bg-[#28c840]" />
              <span className="ml-2 min-w-0 flex-1 truncate rounded-md bg-black/40 px-2 py-1 font-mono text-[10px] text-muted">
                https://app.example/login
              </span>
              <button type="button" onClick={() => setTaken((value) => !value)} className={`${miniOn} shrink-0`}>
                {taken ? "正在接管键鼠" : "点击接管鼠标键盘"}
              </button>
            </div>
            <div className="relative grid min-h-[280px] place-items-center bg-[#0c0d0f] px-6 py-8">
              <div className="w-full max-w-[220px] rounded-2xl border border-white/10 bg-white/[0.03] p-4 text-center">
                <p className="text-sm font-medium text-foreground">扫码登录</p>
                <div className="mx-auto mt-3 grid w-24 grid-cols-5 gap-1" aria-hidden="true">
                  {QR.map((on, index) => (
                    <span key={index} className={`h-3.5 w-3.5 rounded-[2px] ${on ? "bg-foreground" : "bg-white/10"}`} />
                  ))}
                </div>
                <p className="mt-3 text-[11px] leading-5 text-muted">验证码在这台 Linux 的浏览器里</p>
              </div>
              <Cursor className="absolute right-[28%] bottom-[30%] text-foreground" />
              {taken ? (
                <p className="absolute bottom-3 left-3 rounded-full bg-accent px-2.5 py-1 text-[10px] font-semibold text-[#1a1208]">
                  键鼠已接管
                </p>
              ) : null}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function ForkView() {
  const [action, setAction] = useState<null | "rollback" | "fork">(null);

  return (
    <div className="surface-card overflow-hidden">
      <WindowChrome title="回合检查点" subtitle="登录态迁移" trailing="每轮自动留下文件快照" />
      <div className="grid gap-6 p-4 sm:p-6 lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
        <ol className="relative ml-2 border-l border-white/10">
          {TURNS.map((turn) => (
            <li key={turn.n} className="relative pb-5 pl-6 last:pb-0">
              <span
                className={`absolute -left-[5px] top-1.5 h-2.5 w-2.5 rounded-full ${
                  turn.branch ? "bg-accent shadow-[0_0_12px_rgba(224,159,90,0.8)]" : "bg-white/30"
                }`}
              />
              <p className="text-sm text-foreground">
                回合 {turn.n} · {turn.title}
              </p>
              <p className="mt-0.5 text-[11px] text-dim">{turn.meta}</p>
            </li>
          ))}
        </ol>
        <div>
          <div className="flex flex-wrap gap-2">
            <button type="button" onClick={() => setAction("rollback")} className={action === "rollback" ? miniOn : miniOff}>
              回滚到回合 3
            </button>
            <button type="button" onClick={() => setAction("fork")} className={action === "fork" ? miniOn : miniOff}>
              从回合 3 分叉
            </button>
          </div>
          <p className="mt-3 text-xs leading-5 text-muted" aria-live="polite">
            {action === "rollback"
              ? "文件已无损回到回合 3 的快照，后面的改动不再留在工作区。"
              : action === "fork"
                ? "从回合 3 拉出两条会话，让 Claude Code 和 Codex 各试一种拆法。"
                : "每一轮结束都会留下轻量文件检查点。改乱了可以回退，也可以从半路分叉。"}
          </p>
          <div className="mt-4 grid gap-3 sm:grid-cols-2">
            <BranchCard
              engine="Claude Code"
              approach="按模块拆"
              files={["auth/session.ts", "auth/token.ts", "auth/store.ts"]}
              active={action === "fork"}
            />
            <BranchCard
              engine="Codex"
              approach="按请求路径拆"
              files={["routes/session.ts", "routes/refresh.ts"]}
              active={action === "fork"}
            />
          </div>
        </div>
      </div>
    </div>
  );
}

function WindowChrome({ title, subtitle, trailing }: { title: string; subtitle: string; trailing: string }) {
  return (
    <div className="relative flex items-center gap-3 border-b border-white/[0.06] bg-white/[0.03] px-4 py-2.5 backdrop-blur-md">
      <span className="pointer-events-none absolute inset-x-8 top-0 h-px bg-gradient-to-r from-transparent via-white/50 to-transparent" />
      <span className="flex gap-1.5" aria-hidden="true">
        <span className="h-2.5 w-2.5 rounded-full bg-[#ff5f57]" />
        <span className="h-2.5 w-2.5 rounded-full bg-[#febc2e]" />
        <span className="h-2.5 w-2.5 rounded-full bg-[#28c840]" />
      </span>
      <span className="text-xs font-medium text-foreground">{title}</span>
      <span className="hidden text-[11px] text-dim sm:inline">{subtitle}</span>
      <span className="ml-auto rounded-full border border-white/10 bg-black/30 px-2 py-0.5 font-mono text-[10px] text-accent">
        {trailing}
      </span>
    </div>
  );
}

function Machine({
  name,
  meta,
  sessions,
}: {
  name: string;
  meta: string;
  sessions: { title: string; agent: string; state: string; tone: "run" | "wait" | "idle"; active?: boolean }[];
}) {
  return (
    <div className="mt-3">
      <div className="px-2">
        <p className="text-xs font-medium text-foreground">{name}</p>
        <p className="font-mono text-[10px] text-accent">{meta}</p>
      </div>
      {sessions.map((session) => (
        <div
          key={session.title}
          className={`mt-1.5 rounded-xl px-2 py-2 ${session.active ? "bg-white/[0.05] ring-1 ring-white/10" : ""}`}
        >
          <div className="flex items-center gap-2">
            <Dot tone={session.tone} />
            <p className="text-xs text-foreground">{session.title}</p>
          </div>
          <p className="mt-0.5 pl-3.5 text-[10px] text-dim">
            {session.agent} · {session.state}
          </p>
        </div>
      ))}
    </div>
  );
}

function Dot({ tone }: { tone: "run" | "wait" | "idle" }) {
  const color = tone === "run" ? "bg-success" : tone === "wait" ? "bg-accent" : "bg-dim";
  return <span className={`h-1.5 w-1.5 rounded-full ${color} ${tone === "idle" ? "" : "pulse-dot"}`} />;
}

const DIFF = [
  { n: "41", kind: "ctx", text: "export async function loadSession(id: string) {" },
  { n: "42", kind: "del", text: "  const row = db.get(id);" },
  { n: "42", kind: "add", text: "  const row = await store.session(id);" },
  { n: "43", kind: "add", text: "  if (!row) throw new SessionMissing(id);" },
  { n: "44", kind: "ctx", text: "  return toView(row);" },
] as const;

function DiffLines() {
  return (
    <div className="overflow-x-auto font-mono text-[11px] leading-6">
      {DIFF.map((line) => (
        <div key={`${line.kind}-${line.n}-${line.text}`} className={`grid grid-cols-[2.5rem_minmax(0,1fr)] ${lineTone(line.kind)}`}>
          <span className="px-2 text-right text-dim">{line.n}</span>
          <span className="pr-3 whitespace-pre">{line.kind === "add" ? "+ " : line.kind === "del" ? "− " : "  "}{line.text}</span>
        </div>
      ))}
    </div>
  );
}

function lineTone(kind: "ctx" | "add" | "del") {
  if (kind === "add") return "bg-[#8fbf9f]/10 text-[#c9ead6]";
  if (kind === "del") return "bg-[#e07a6a]/10 text-[#f3c2ba]";
  return "text-muted";
}

function DecisionLabel({ decision }: { decision: Decision }) {
  if (decision === "pending") return null;
  return (
    <p className={`mt-2 text-[11px] ${decision === "approved" ? "text-success" : "text-danger"}`} aria-live="polite">
      {decision === "approved" ? "已批准 · 正在执行" : "已拒绝 · 停在这一步"}
    </p>
  );
}

function PhoneFrame({ children, caption }: { children: ReactNode; caption?: string }) {
  return (
    <div className="mx-auto w-full max-w-[260px]">
      <div className="rounded-[1.9rem] border border-white/10 bg-[#0b0c0e] p-[7px] shadow-[0_30px_70px_-28px_rgba(0,0,0,0.9),0_0_0_1px_rgba(255,255,255,0.04)]">
        <div className="overflow-hidden rounded-[1.45rem] border border-white/[0.06] bg-[#121316]">
          <div className="relative flex items-center justify-between px-4 pt-2 pb-1 font-mono text-[10px] text-dim">
            <span>09:41</span>
            <span className="absolute left-1/2 h-4 w-16 -translate-x-1/2 rounded-full bg-black" />
            <span>5G</span>
          </div>
          {children}
        </div>
      </div>
      {caption ? <p className="mt-2 text-center text-[11px] text-dim">{caption}</p> : null}
    </div>
  );
}

function BranchCard({
  engine,
  approach,
  files,
  active,
}: {
  engine: string;
  approach: string;
  files: string[];
  active: boolean;
}) {
  return (
    <article className={`rounded-xl border p-3 ${active ? "border-accent/40 bg-accent/[0.05]" : "border-white/[0.06] bg-black/25"}`}>
      <p className="text-sm font-medium text-foreground">{engine}</p>
      <p className="mt-0.5 text-[11px] text-accent">{approach}</p>
      <ul className="mt-2 space-y-1 font-mono text-[10px] text-muted">
        {files.map((file) => (
          <li key={file}>{file}</li>
        ))}
      </ul>
    </article>
  );
}

function TabIcon({ id }: { id: TabId }) {
  const common = "h-4 w-4 shrink-0";
  if (id === "desktop") {
    return (
      <svg viewBox="0 0 24 24" className={common} fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
        <rect x="3" y="4" width="18" height="12" rx="2" />
        <path d="M8 20h8M12 16v4" strokeLinecap="round" />
      </svg>
    );
  }
  if (id === "mobile") {
    return (
      <svg viewBox="0 0 24 24" className={common} fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
        <rect x="7" y="2.5" width="10" height="19" rx="2" />
        <path d="M11 18.5h2" strokeLinecap="round" />
      </svg>
    );
  }
  if (id === "remote") {
    return (
      <svg viewBox="0 0 24 24" className={common} fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
        <rect x="3" y="4" width="18" height="14" rx="2" />
        <path d="M3 8h18" />
      </svg>
    );
  }
  return (
    <svg viewBox="0 0 24 24" className={common} fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
      <circle cx="6" cy="6" r="2" />
      <circle cx="6" cy="18" r="2" />
      <circle cx="18" cy="12" r="2" />
      <path d="M6 8v8M8 6c6 0 8 4 8 6" />
    </svg>
  );
}

function Cursor({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" className={`h-5 w-5 ${className ?? ""}`} fill="currentColor" aria-hidden="true">
      <path d="M5 3l12 9.2-5.2.6 3.4 6.2-2.2 1.2-3.4-6.1L5 17.2V3z" />
    </svg>
  );
}

const TURNS = [
  { n: "1", title: "读完认证模块", meta: "检查点 · 12 个文件" },
  { n: "2", title: "补上迁移测试", meta: "检查点 · 14 个文件" },
  { n: "3", title: "抽出会话层", meta: "可以从这里回退或分叉", branch: true },
  { n: "4", title: "还没发生", meta: "主线停在回合 3 之后" },
];

const QR = [
  1, 1, 1, 0, 1, 1, 0, 1, 0, 1, 1, 0, 1, 1, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 1,
];

const miniOn =
  "rounded-lg bg-accent px-2.5 py-1 text-[11px] font-semibold text-[#1a1208] transition hover:brightness-110";
const miniOff =
  "rounded-lg border border-white/10 bg-white/[0.03] px-2.5 py-1 text-[11px] text-muted transition hover:border-white/25 hover:text-foreground";
