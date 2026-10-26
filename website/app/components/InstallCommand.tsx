"use client";

import { useEffect, useRef, useState } from "react";

const COMMANDS = {
  standard:
    "curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash",
  desktop:
    "curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash -s -- --with-desktop",
} as const;

type Mode = keyof typeof COMMANDS;

const MODES: { id: Mode; label: string; flag?: string; detail: string }[] = [
  {
    id: "standard",
    label: "标准模式",
    detail: "只接入智能体和终端。代码与环境留在远程开发机上全速运行。",
  },
  {
    id: "desktop",
    label: "含远程桌面与浏览器",
    flag: "--with-desktop",
    detail: "同时准备远程桌面和浏览器。扫码登录或界面验证时，可以在桌面上直接接管。",
  },
];

export function InstallCommand() {
  const [mode, setMode] = useState<Mode>("standard");
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | null>(null);
  const command = COMMANDS[mode];
  const detail = MODES.find((item) => item.id === mode)?.detail;

  useEffect(() => {
    return () => {
      if (timer.current) window.clearTimeout(timer.current);
    };
  }, []);

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      if (timer.current) window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  }

  function select(next: Mode) {
    setMode(next);
    setCopied(false);
  }

  return (
    <div className="surface-card flex h-full flex-col overflow-hidden">
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-white/[0.06] px-3 py-3 sm:px-4">
        <p className="text-sm font-medium text-foreground">Linux 远程开发机一键接入</p>
        <div className="grid w-full grid-cols-1 gap-1 rounded-xl bg-black/30 p-1 sm:w-auto sm:grid-cols-2">
          {MODES.map((item) => {
            const active = mode === item.id;
            return (
              <button
                key={item.id}
                type="button"
                onClick={() => select(item.id)}
                aria-pressed={active}
                className={`rounded-lg px-3 py-1.5 text-left transition-colors ${
                  active ? "bg-white/[0.07] text-accent" : "text-dim hover:text-foreground"
                }`}
              >
                <span className="block text-[12px] font-medium leading-4">{item.label}</span>
                {item.flag ? (
                  <span className="mt-0.5 block font-mono text-[10px] text-dim">{item.flag}</span>
                ) : (
                  <span className="mt-0.5 block text-[10px] text-dim">智能体与终端</span>
                )}
              </button>
            );
          })}
        </div>
      </div>
      <div className="flex items-center gap-3 px-3 py-3 sm:px-4">
        <div className="min-w-0 flex-1 overflow-x-auto">
          <code className="block whitespace-nowrap font-mono text-[12.5px] leading-6 text-foreground">
            <span className="text-dim">$ </span>
            {command}
          </code>
        </div>
        <button
          type="button"
          onClick={copy}
          aria-live="polite"
          className={`shrink-0 rounded-lg border px-3 py-1.5 text-xs font-medium transition-colors ${
            copied
              ? "border-success/40 bg-success/10 text-success"
              : "border-white/10 bg-white/[0.04] text-foreground hover:border-accent/50 hover:text-accent"
          }`}
        >
          {copied ? "已复制" : "复制"}
        </button>
      </div>
      <p className="border-t border-white/[0.04] px-3 py-2.5 text-xs leading-5 text-muted sm:px-4">{detail}</p>
    </div>
  );
}
