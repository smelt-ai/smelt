"use client";

import Image from "next/image";
import Link from "next/link";
import { useState } from "react";
import { GitHubIcon } from "./icons";

const REPO_URL = "https://github.com/smelt-ai/smelt";
const RELEASE_URL = "https://github.com/smelt-ai/smelt/releases/latest";

const LINKS = [
  { href: "/#features", label: "核心特性" },
  { href: "/#preview", label: "多端预览" },
  { href: "/#agents", label: "智能体生态" },
  { href: "/docs", label: "使用文档" },
  { href: "/develop", label: "参与开发" },
];

export function Nav() {
  const [open, setOpen] = useState(false);

  return (
    <header className="sticky top-0 z-50 border-b border-border bg-background/95">
      <div className="mx-auto flex h-14 max-w-6xl items-center justify-between gap-4 px-6">
        <Link href="/" className="flex items-center gap-2">
          <Image src="/icon.svg" alt="" width={22} height={22} unoptimized />
          <span className="font-mono text-sm font-semibold tracking-tight text-foreground">
            smelt
          </span>
        </Link>

        <nav className="hidden items-center gap-5 text-sm text-muted lg:flex">
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

        <div className="flex items-center gap-2">
          <a
            href={REPO_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="hidden items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs font-medium text-foreground transition-colors hover:border-accent/60 hover:text-accent sm:flex"
          >
            <GitHubIcon className="h-3.5 w-3.5" />
            GitHub
          </a>
          <a
            href={RELEASE_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="rounded-md bg-accent px-3 py-1.5 text-xs font-semibold text-[#1a1208] transition-opacity hover:opacity-90"
          >
            下载
          </a>
          <button
            type="button"
            className="rounded-md border border-border px-2 py-1.5 font-mono text-[11px] text-muted lg:hidden"
            aria-expanded={open}
            aria-label={open ? "关闭导航" : "打开导航"}
            onClick={() => setOpen((value) => !value)}
          >
            {open ? "关闭" : "菜单"}
          </button>
        </div>
      </div>

      {open ? (
        <nav className="border-t border-border bg-background px-6 py-3 lg:hidden">
          <div className="mx-auto flex max-w-6xl flex-col gap-1">
            {LINKS.map((link) => (
              <Link
                key={link.href}
                href={link.href}
                onClick={() => setOpen(false)}
                className="rounded-md px-2 py-2 text-sm text-muted hover:bg-surface hover:text-foreground"
              >
                {link.label}
              </Link>
            ))}
            <a
              href={REPO_URL}
              target="_blank"
              rel="noopener noreferrer"
              className="rounded-md px-2 py-2 text-sm text-muted hover:bg-surface hover:text-foreground"
            >
              GitHub 仓库
            </a>
          </div>
        </nav>
      ) : null}
    </header>
  );
}
