"use client";

import { useState } from "react";

const TABS = [
  { id: "desktop", label: "GPUI 原生桌面工作台" },
  { id: "remote", label: "跨机器直连 & 内嵌远程桌面" },
  { id: "mobile", label: "手机 / 平板「出菜口」" },
] as const;

type TabId = (typeof TABS)[number]["id"];

export function CockpitPreview() {
  const [tab, setTab] = useState<TabId>("desktop");

  return (
    <div id="preview" className="mt-12 scroll-mt-20">
      <div
        role="tablist"
        aria-label="产品视角"
        className="flex flex-wrap gap-1 border-b border-border"
      >
        {TABS.map((item) => {
          const active = tab === item.id;
          return (
            <button
              key={item.id}
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => setTab(item.id)}
              className={`border-b px-3 py-2 font-mono text-[12px] transition-colors ${
                active
                  ? "border-accent text-accent"
                  : "border-transparent text-dim hover:text-foreground"
              }`}
            >
              {item.label}
            </button>
          );
        })}
      </div>

      <div className="mt-4 overflow-hidden rounded-lg border border-border bg-surface">
        <div className="flex items-center gap-2 border-b border-border bg-surface-2 px-3 py-2">
          <span className="h-1.5 w-1.5 rounded-full bg-accent" />
          <span className="font-mono text-[11px] text-dim">
            {tab === "desktop" && "smelt · MacBook Pro"}
            {tab === "remote" && "smelt · gpu-box-ubuntu · iroh QUIC"}
            {tab === "mobile" && "smelt · 出菜口"}
          </span>
          <span className="ml-auto font-mono text-[10px] text-dim">Control API v3</span>
        </div>
        {tab === "desktop" ? <DesktopView /> : null}
        {tab === "remote" ? <RemoteView /> : null}
        {tab === "mobile" ? <MobileView /> : null}
      </div>
    </div>
  );
}

function DesktopView() {
  return (
    <div className="grid min-h-[420px] lg:grid-cols-[196px_minmax(0,1fr)_220px]">
      <aside className="border-b border-border p-3 lg:border-r lg:border-b-0">
        <p className="font-mono text-[10px] tracking-widest text-dim">机器</p>
        <p className="mt-3 text-xs font-medium text-foreground">MacBook Pro（本机）</p>
        <Row active label="Pi · smelt" meta="在跑" />
        <Row label="Claude · api" meta="空闲" />
        <p className="mt-4 text-xs font-medium text-foreground">gpu-box-ubuntu</p>
        <p className="font-mono text-[10px] text-accent">iroh P2P</p>
        <Row label="Codex · train" meta="等审批" />
        <p className="mt-4 font-mono text-[10px] tracking-widest text-dim">最近对话</p>
        <Row label="修发布脚本" meta="gpu-box · 2 分钟" />
        <Row label="对照 schema v24" meta="本机 · 18 分钟" />
      </aside>

      <div className="border-b border-border p-4 lg:border-r lg:border-b-0">
        <div className="rounded-md border border-border bg-background px-3 py-2">
          <div className="flex items-center justify-between font-mono text-[11px] text-dim">
            <span>思考 · 已折叠</span>
            <span>1.8s</span>
          </div>
        </div>
        <div className="mt-3 rounded-md border border-border bg-background">
          <div className="flex items-center justify-between border-b border-border px-3 py-1.5 font-mono text-[11px]">
            <span className="text-foreground">scripts/install-smeltd.sh</span>
            <span className="text-success">+18</span>
            <span className="text-danger">−2</span>
          </div>
          <pre className="overflow-x-auto px-3 py-2 font-mono text-[11px] leading-5 text-muted">
            <span className="text-success">+ smeltd mcp add playwright</span>
            {"\n"}
            <span className="text-dim">  # CDP 9222 · 常驻 Chrome</span>
          </pre>
        </div>
        <div className="mt-3 rounded-md border border-accent/40 bg-background px-3 py-2">
          <div className="flex items-center justify-between gap-3">
            <div>
              <p className="font-mono text-[11px] text-accent">Bash · 等待批准</p>
              <p className="mt-1 font-mono text-[12px] text-foreground">
                systemctl --user restart smelt-chrome
              </p>
            </div>
            <span className="rounded-md bg-accent px-2 py-1 font-mono text-[10px] text-[#1a1208]">
              批准
            </span>
          </div>
        </div>
        <div className="mt-3 flex items-center justify-between font-mono text-[11px] text-dim">
          <span>回合 14 · 检查点</span>
          <span className="rounded-md border border-border px-2 py-1 text-foreground">
            回滚到此检查点
          </span>
        </div>
      </div>

      <aside className="p-3">
        <p className="font-mono text-[10px] tracking-widest text-dim">子智能体</p>
        <div className="mt-2 rounded-md border border-border bg-background p-2">
          <p className="text-xs text-foreground">查 apt 镜像回退</p>
          <p className="mt-1 font-mono text-[10px] text-success">完成 · 12s</p>
        </div>
        <div className="mt-2 rounded-md border border-border bg-background p-2">
          <p className="text-xs text-foreground">核对 Xvnc 几何</p>
          <p className="mt-1 font-mono text-[10px] text-accent">并行中</p>
        </div>
        <p className="mt-4 font-mono text-[10px] tracking-widest text-dim">Plan</p>
        <ul className="mt-2 space-y-1.5 font-mono text-[11px] text-muted">
          <li className="text-success">✓ 装 smeltd</li>
          <li className="text-success">✓ pair</li>
          <li className="text-accent">▸ playwright 预设</li>
          <li>○ 打开桌面页签</li>
        </ul>
      </aside>
    </div>
  );
}

function RemoteView() {
  return (
    <div className="grid min-h-[420px] lg:grid-cols-2">
      <div className="border-b border-border p-4 lg:border-r lg:border-b-0">
        <div className="flex items-center justify-between font-mono text-[11px]">
          <span className="text-foreground">playwright MCP</span>
          <span className="text-accent">CDP 127.0.0.1:9222</span>
        </div>
        <ol className="mt-4 space-y-2 font-mono text-[12px] text-muted">
          <li className="rounded-md border border-border bg-background px-3 py-2">
            <span className="text-dim">01 </span>navigate · 发布页
          </li>
          <li className="rounded-md border border-border bg-background px-3 py-2">
            <span className="text-dim">02 </span>click · Assets / smeltd
          </li>
          <li className="rounded-md border border-accent/40 bg-background px-3 py-2 text-foreground">
            <span className="text-accent">03 </span>等待人接管验证码
          </li>
        </ol>
        <p className="mt-4 font-mono text-[11px] text-dim">
          中继 · relay · 168 ms · 打洞未成，走自建 iroh relay
        </p>
      </div>
      <div className="bg-background p-4">
        <div className="flex items-center justify-between font-mono text-[11px] text-dim">
          <span>noVNC · XFCE</span>
          <span>1440×900 · :5999</span>
        </div>
        <div className="mt-3 overflow-hidden rounded-md border border-border">
          <div className="flex h-7 items-center gap-2 bg-surface-2 px-2">
            <span className="h-2 w-2 rounded-full bg-dim" />
            <span className="font-mono text-[10px] text-muted">Chrome · 常驻</span>
          </div>
          <div className="grid h-52 place-items-center bg-[#0c0d0f] px-6 text-center">
            <div>
              <p className="font-mono text-[12px] text-foreground">gpu-box-ubuntu</p>
              <p className="mt-2 text-xs leading-5 text-muted">
                同一块虚拟屏幕。智能体操作的 Chrome 和你接管的窗口是同一个实例。
              </p>
            </div>
          </div>
        </div>
        <div className="mt-3 flex gap-2 font-mono text-[10px]">
          <span className="rounded-md bg-accent px-2 py-1 text-[#1a1208]">接管</span>
          <span className="rounded-md border border-border px-2 py-1 text-muted">只看</span>
        </div>
      </div>
    </div>
  );
}

function MobileView() {
  return (
    <div className="grid gap-6 bg-background p-4 sm:p-6 lg:grid-cols-[280px_minmax(0,1fr)]">
      <div className="mx-auto w-full max-w-[280px] overflow-hidden rounded-[28px] border border-border bg-surface">
        <div className="flex items-center justify-between px-4 pt-3 font-mono text-[10px] text-dim">
          <span>09:41</span>
          <span>出菜口</span>
        </div>
        <div className="px-3 pt-3">
          <p className="font-mono text-[10px] tracking-widest text-accent">待拍板 · 2</p>
          <article className="mt-2 rounded-md border border-accent/40 bg-background p-2.5">
            <p className="text-xs text-foreground">批准危险命令</p>
            <p className="mt-1 font-mono text-[10px] text-dim">gpu-box · rm -rf dist</p>
            <div className="mt-2 flex gap-1.5">
              <span className="rounded-md bg-accent px-2 py-0.5 font-mono text-[10px] text-[#1a1208]">
                批准
              </span>
              <span className="rounded-md border border-border px-2 py-0.5 font-mono text-[10px] text-muted">
                拒绝
              </span>
            </div>
          </article>
          <article className="mt-2 rounded-md border border-border bg-background p-2.5">
            <p className="text-xs text-foreground">架构选型</p>
            <p className="mt-1 text-[11px] leading-5 text-muted">WAL 双库还是单库？</p>
            <span className="mt-2 inline-block rounded-md border border-border px-2 py-0.5 font-mono text-[10px] text-foreground">
              回复：保持双库
            </span>
          </article>
        </div>
        <div className="mt-3 border-t border-border px-3 py-3">
          <p className="font-mono text-[10px] tracking-widest text-dim">各机器</p>
          <p className="mt-2 text-xs text-foreground">MacBook Pro</p>
          <p className="font-mono text-[10px] text-success">刚完成 · 检查点已写</p>
          <p className="mt-2 text-xs text-foreground">gpu-box-ubuntu</p>
          <p className="font-mono text-[10px] text-accent">执行中 · playwright</p>
        </div>
        <div className="grid grid-cols-4 border-t border-border text-center font-mono text-[10px]">
          <span className="bg-surface-2 py-2 text-accent">指挥台</span>
          <span className="py-2 text-dim">会话</span>
          <span className="py-2 text-dim">智能体</span>
          <span className="py-2 text-dim">设置</span>
        </div>
      </div>
      <div className="flex flex-col justify-center">
        <p className="font-mono text-[11px] tracking-widest text-dim">同时连接</p>
        <ul className="mt-4 space-y-3">
          <Machine name="MacBook Pro" state="本机 · Unix socket" />
          <Machine name="gpu-box-ubuntu" state="iroh P2P · 在线" />
          <Machine name="lab-a100" state="iroh P2P · 重连中" />
        </ul>
        <p className="mt-6 max-w-md text-sm leading-6 text-muted">
          启动后连上所有已配对机器。首屏只放待审批、待回答，以及每台机器正在做和刚做完的事。平板是导航、列表、详情三栏。
        </p>
      </div>
    </div>
  );
}

function Row({
  label,
  meta,
  active,
}: {
  label: string;
  meta: string;
  active?: boolean;
}) {
  return (
    <div
      className={`mt-1.5 rounded-md px-2 py-1.5 ${
        active ? "bg-surface-3" : ""
      }`}
    >
      <p className="text-xs text-foreground">{label}</p>
      <p className="font-mono text-[10px] text-dim">{meta}</p>
    </div>
  );
}

function Machine({ name, state }: { name: string; state: string }) {
  return (
    <li className="border-l border-accent/50 pl-3">
      <p className="text-sm text-foreground">{name}</p>
      <p className="font-mono text-[11px] text-dim">{state}</p>
    </li>
  );
}
