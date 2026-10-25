import { InstallCommand } from "./InstallCommand";
import { CockpitPreview } from "./CockpitPreview";

const RELEASE_URL = "https://github.com/smelt-ai/smelt/releases/latest";

export function Hero() {
  return (
    <section className="mx-auto max-w-6xl px-6 pt-16 pb-8 sm:pt-20">
      <div className="mb-6 inline-flex items-center gap-2 rounded-full border border-border bg-surface px-3 py-1 font-mono text-[11px] text-muted">
        <span className="h-1.5 w-1.5 rounded-full bg-accent" />
        v2026.10 · 跨机器智能体管理 · 手机端「出菜口」指挥台 · 单二进制 smelt-pi
      </div>

      <h1 className="max-w-4xl text-4xl font-semibold leading-[1.12] tracking-tight text-foreground sm:text-5xl lg:text-[3.4rem]">
        在同一套工作台里，
        <span className="text-accent">指挥你所有机器上的 AI 编程智能体</span>
      </h1>

      <p className="mt-5 max-w-2xl text-base leading-7 text-muted sm:text-[17px]">
        无论是本机 Mac、实验室或云端 Linux 服务器，还是随身手机「出菜口」——结构化对话、长期记忆、定时自动化与内嵌远程桌面在同一套指挥台里完成。
      </p>

      <div className="mt-8 grid gap-4 lg:grid-cols-[auto_minmax(0,1fr)] lg:items-start">
        <div className="flex flex-wrap items-center gap-3">
          <a
            href={RELEASE_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="rounded-md bg-accent px-4 py-2.5 text-sm font-semibold text-[#1a1208] transition-opacity hover:opacity-90"
          >
            下载 macOS 桌面端
          </a>
          <a
            href="/docs#手机出菜口"
            className="rounded-md border border-border px-4 py-2.5 text-sm font-medium text-foreground transition-colors hover:border-accent/60 hover:text-accent"
          >
            下载移动端
          </a>
        </div>
        <InstallCommand />
      </div>

      <CockpitPreview />
    </section>
  );
}
