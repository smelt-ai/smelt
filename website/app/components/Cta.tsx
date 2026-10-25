import { InstallCommand } from "./InstallCommand";

const RELEASE_URL = "https://github.com/smelt-ai/smelt/releases/latest";

export function Cta() {
  return (
    <section id="start" className="mx-auto max-w-6xl scroll-mt-20 px-6 py-20">
      <div className="rounded-lg border border-border bg-surface px-6 py-8 sm:px-8">
        <p className="font-mono text-[11px] tracking-[0.18em] text-accent">开始</p>
        <h2 className="mt-3 max-w-xl text-3xl font-semibold tracking-tight text-foreground">
          先装桌面，或先把一台 Linux 接进来
        </h2>
        <p className="mt-3 max-w-2xl text-sm leading-6 text-muted">
          macOS 桌面端在 GitHub Releases 的 Smelt.dmg。Android 每次发版提供签名包
          Smelt-&lt;版本&gt;-android-arm64.apk。iOS 用 make ios 打 TestFlight 归档。
        </p>

        <div className="mt-6 flex flex-wrap gap-3">
          <a
            href={RELEASE_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="rounded-md bg-accent px-4 py-2.5 text-sm font-semibold text-[#1a1208] hover:opacity-90"
          >
            下载 macOS .dmg
          </a>
          <a
            href="/docs#手机出菜口"
            className="rounded-md border border-border px-4 py-2.5 text-sm text-foreground hover:border-accent/60 hover:text-accent"
          >
            Android / iOS
          </a>
          <a
            href="/docs"
            className="rounded-md border border-border px-4 py-2.5 text-sm text-foreground hover:border-accent/60 hover:text-accent"
          >
            使用文档
          </a>
          <a
            href="/develop"
            className="rounded-md border border-border px-4 py-2.5 text-sm text-foreground hover:border-accent/60 hover:text-accent"
          >
            参与开发
          </a>
        </div>

        <div className="mt-6">
          <InstallCommand />
        </div>
      </div>
    </section>
  );
}
