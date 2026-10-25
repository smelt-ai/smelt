"use client";

import Image from "next/image";
import Link from "next/link";
import { useState } from "react";
import { GitHubIcon } from "./icons";

const REPO_URL = "https://github.com/smelt-ai/smelt";
const RELEASE_URL = "https://github.com/smelt-ai/smelt/releases/latest";

const LINKS = [
  { href: "/#features", label: "核心能力" },
  { href: "/#preview", label: "多端体验" },
  { href: "/#guide", label: "上手指南" },
  { href: "/#agents", label: "智能体生态" },
  { href: "/docs", label: "使用文档" },
  { href: "/develop", label: "参与开发" },
];

export function Nav() {
  const [open, setOpen] = useState(false);

  return (
    <header className="sticky top-0 z-50 border-b border-white/[0.06] bg-[#08090a]/75 backdrop-blur-md">
      <div className="mx-auto flex h-16 max-w-6xl items-center gap-4 px-6">
        <Link href="/" className="flex shrink-0 items-center gap-2">
          <Image src="/icon.svg" alt="" width={22} height={22} unoptimized />
          <span className="text-[15px] font-semibold tracking-tight text-foreground">smelt</span>
        </Link>

        <nav className="ml-auto hidden items-center gap-5 text-[13px] text-muted lg:flex">
          {LINKS.map((link) => (
            <Link
              key={link.href}
              href={link.href}
              className="transition-colors hover:text-foreground"
            >
              {link.label}
            </Link>
          ))}
        </nav>

        <div className="flex items-center gap-2 lg:ml-2">
          <a
            href={REPO_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="hidden items-center gap-1.5 rounded-xl border border-white/10 bg-white/[0.03] px-2.5 py-1.5 text-xs font-medium text-foreground transition-colors hover:border-accent/50 hover:text-accent sm:flex"
          >
            <GitHubIcon className="h-3.5 w-3.5" />
            GitHub
          </a>
          <a
            href={RELEASE_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="rounded-xl bg-accent px-3 py-1.5 text-xs font-semibold text-[#1a1208] shadow-[inset_0_1px_0_rgba(255,255,255,0.4)] transition hover:brightness-110"
          >
            免费下载
          </a>
          <button
            type="button"
            className="grid h-9 w-9 place-items-center rounded-xl border border-white/10 text-muted lg:hidden"
            aria-expanded={open}
            aria-controls="mobile-nav"
            aria-label={open ? "关闭导航" : "打开导航"}
            onClick={() => setOpen((value) => !value)}
          >
            <span className="sr-only">{open ? "关闭" : "菜单"}</span>
            <svg viewBox="0 0 20 20" className="h-4 w-4" fill="none" aria-hidden="true">
              {open ? (
                <path d="M5 5l10 10M15 5L5 15" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
              ) : (
                <path d="M3.5 6h13M3.5 10h13M3.5 14h13" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
              )}
            </svg>
          </button>
        </div>
      </div>

      {open ? (
        <nav id="mobile-nav" className="border-t border-white/[0.06] bg-[#08090a]/95 px-6 py-3 lg:hidden">
          <div className="mx-auto flex max-w-6xl flex-col gap-1">
            {LINKS.map((link) => (
              <Link
                key={link.href}
                href={link.href}
                onClick={() => setOpen(false)}
                className="rounded-xl px-2 py-2.5 text-sm text-muted hover:bg-white/[0.04] hover:text-foreground"
              >
                {link.label}
              </Link>
            ))}
            <a
              href={REPO_URL}
              target="_blank"
              rel="noopener noreferrer"
              className="rounded-xl px-2 py-2.5 text-sm text-muted hover:bg-white/[0.04] hover:text-foreground sm:hidden"
            >
              GitHub
            </a>
          </div>
        </nav>
      ) : null}
    </header>
  );
}
