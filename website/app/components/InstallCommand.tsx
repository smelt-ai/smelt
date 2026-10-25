"use client";

import { useState } from "react";

const COMMANDS = {
  headless:
    "curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash",
  desktop:
    "curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash -s -- --with-desktop",
} as const;

type Mode = keyof typeof COMMANDS;

export function InstallCommand() {
  const [mode, setMode] = useState<Mode>("headless");
  const [copied, setCopied] = useState(false);
  const command = COMMANDS[mode];

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  }

  return (
    <div className="overflow-hidden rounded-lg border border-border bg-surface">
      <div className="flex items-center gap-1 border-b border-border px-2 py-1.5">
        <ModeButton
          active={mode === "headless"}
          onClick={() => setMode("headless")}
        >
          标准无头
        </ModeButton>
        <ModeButton
          active={mode === "desktop"}
          onClick={() => setMode("desktop")}
        >
          带远程桌面
        </ModeButton>
        <span className="ml-auto hidden font-mono text-[10px] tracking-wide text-dim sm:inline">
          Linux · install.sh
        </span>
      </div>
      <div className="flex items-start gap-3 px-3 py-3">
        <code className="min-w-0 flex-1 font-mono text-[12px] leading-5 break-all text-foreground">
          <span className="text-dim">$ </span>
          {command}
        </code>
        <button
          type="button"
          onClick={copy}
          className="shrink-0 rounded-md border border-border bg-surface-2 px-2.5 py-1 font-mono text-[11px] text-foreground transition-colors hover:border-accent/60 hover:text-accent"
        >
          {copied ? "已复制" : "复制"}
        </button>
      </div>
    </div>
  );
}

function ModeButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`rounded-md px-2.5 py-1 font-mono text-[11px] transition-colors ${
        active
          ? "bg-surface-3 text-accent"
          : "text-dim hover:text-foreground"
      }`}
      aria-pressed={active}
    >
      {children}
    </button>
  );
}
