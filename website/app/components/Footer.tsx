import Image from "next/image";
import Link from "next/link";

const REPO_URL = "https://github.com/smelt-ai/smelt";

const LINKS = [
  { href: "/docs", label: "使用文档", external: false },
  { href: "/develop", label: "参与开发", external: false },
  { href: REPO_URL, label: "源码", external: true },
  { href: `${REPO_URL}/releases`, label: "Releases", external: true },
  { href: `${REPO_URL}/issues`, label: "问题反馈", external: true },
];

export function Footer() {
  return (
    <footer className="border-t border-white/[0.06]">
      <div className="mx-auto flex max-w-6xl flex-col gap-8 px-6 py-12 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <div className="flex items-center gap-2">
            <Image src="/icon.svg" alt="" width={20} height={20} unoptimized />
            <span className="text-sm font-semibold tracking-tight text-foreground">smelt</span>
          </div>
          <p className="mt-3 max-w-xs text-sm leading-6 text-muted">
            原生 macOS 工作台、Linux 远程开发机，以及 iOS / Android 随身端。
          </p>
          <p className="mt-4 font-mono text-xs text-dim">Apache-2.0 License</p>
        </div>
        <nav className="flex flex-wrap gap-x-6 gap-y-3 text-sm text-muted">
          {LINKS.map((link) =>
            link.external ? (
              <a
                key={link.label}
                href={link.href}
                target="_blank"
                rel="noopener noreferrer"
                className="transition-colors hover:text-foreground"
              >
                {link.label}
              </a>
            ) : (
              <Link key={link.label} href={link.href} className="transition-colors hover:text-foreground">
                {link.label}
              </Link>
            ),
          )}
        </nav>
      </div>
    </footer>
  );
}
