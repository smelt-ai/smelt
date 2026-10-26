import Link from "next/link";
import { AppleLogo, BookIcon, DesktopIcon, PhoneIcon } from "./icons";
import { InstallCommand } from "./InstallCommand";
import { CockpitPreview } from "./CockpitPreview";

const RELEASE_URL = "https://github.com/smelt-ai/smelt/releases/latest";

export function Hero() {
  return (
    <section className="relative">
      <div className="hero-stage" aria-hidden="true">
        <div className="hero-glow" />
        <div className="hero-dots" />
        <div className="hero-grid" />
      </div>

      <div className="relative z-10 mx-auto max-w-6xl px-6 pt-16 pb-6 sm:pt-20">
        <div className="pill max-w-full flex-wrap">
          <span className="pulse-dot h-1.5 w-1.5 shrink-0 rounded-full bg-accent shadow-[0_0_12px_rgba(224,159,90,0.8)]" />
          <span className="leading-5">
            macOS 与 Windows 工作台 · iOS 与 Android 随身掌控 · Linux 远程开发机直连
          </span>
        </div>

        <h1 className="mt-6 max-w-4xl text-[2.35rem] font-semibold leading-[1.12] tracking-tight text-foreground sm:text-5xl lg:text-[3.45rem]">
          <span className="block">把你所有机器上的 AI 编程智能体，</span>
          <span className="mt-1 block text-accent">收进同一个原生工作台</span>
        </h1>

        <p className="mt-6 max-w-3xl text-base leading-8 text-muted sm:text-lg">
          代码可以留在本机 Mac、本机 Windows，或实验室和云端的 Linux 服务器上：在一个原生桌面工作台里并行指挥
          Claude Code、Codex 与内置智能体；离开电脑后，打开手机就能审批权限、查看 Diff、接管任务。
        </p>

        <div className="mt-10 grid items-stretch gap-4 lg:grid-cols-[minmax(280px,340px)_minmax(0,1fr)]">
          <div className="flex flex-col gap-3">
            <a href={RELEASE_URL} target="_blank" rel="noopener noreferrer" className="btn btn-primary">
              <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-black/10">
                <AppleLogo className="h-5 w-5" />
              </span>
              <span>
                <span className="block text-sm font-semibold leading-5">下载 macOS 版</span>
                <span className="block text-[11px] font-medium leading-4 text-[#1a1208]/70">
                  Apple Silicon · 免费
                </span>
              </span>
            </a>
            <a href={RELEASE_URL} target="_blank" rel="noopener noreferrer" className="btn btn-secondary">
              <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg border border-white/10 bg-white/[0.03]">
                <DesktopIcon className="h-4 w-4" />
              </span>
              <span>
                <span className="block text-sm font-semibold leading-5">下载 Windows 版</span>
                <span className="block text-[11px] font-medium leading-4 text-dim">
                  Windows 10 1803+ · x86_64 · 免安装便携包
                </span>
              </span>
            </a>
            <Link href="/docs#远程访问" className="btn btn-secondary">
              <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg border border-white/10 bg-white/[0.03]">
                <PhoneIcon className="h-4 w-4" />
              </span>
              <span>
                <span className="block text-sm font-semibold leading-5">iOS / Android 移动端</span>
                <span className="block text-[11px] font-medium leading-4 text-dim">手机与平板随身掌控</span>
              </span>
            </Link>
            <Link href="/docs#快速开始" className="btn btn-secondary">
              <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg border border-white/10 bg-white/[0.03]">
                <BookIcon className="h-4 w-4" />
              </span>
              <span>
                <span className="block text-sm font-semibold leading-5">快速上手文档</span>
                <span className="block text-[11px] font-medium leading-4 text-dim">安装、配对、远程桌面</span>
              </span>
            </Link>
          </div>
          <InstallCommand />
        </div>

        <CockpitPreview />
      </div>
    </section>
  );
}
