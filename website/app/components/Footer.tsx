import Link from "next/link";
import { GitHubIcon } from "./icons";

const REPO_URL = "https://github.com/smelt-ai/smelt";

export function Footer() {
  return (
    <footer className="border-t border-border">
      <div className="mx-auto flex max-w-6xl flex-col gap-6 px-6 py-10 text-sm text-dim sm:flex-row sm:items-start sm:justify-between">
        <div>
          <p className="font-mono text-foreground/80">smelt · Apache-2.0 License</p>
          <p className="mt-2 max-w-sm text-xs leading-5">
            macOS 桌面、Linux 上的 smeltd、iOS / Android「出菜口」。
          </p>
        </div>
        <div className="flex flex-wrap gap-x-5 gap-y-2 text-xs">
          <Link href="/docs" className="hover:text-foreground">
            使用文档
          </Link>
          <Link href="/develop" className="hover:text-foreground">
            参与开发
          </Link>
          <a href={`${REPO_URL}/releases/latest`} className="hover:text-foreground">
            Releases
          </a>
          <a
            href={REPO_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1.5 hover:text-foreground"
          >
            <GitHubIcon className="h-3.5 w-3.5" />
            GitHub
          </a>
        </div>
      </div>
    </footer>
  );
}
