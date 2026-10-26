import Link from "next/link";
import { AppleLogo, DesktopIcon, PhoneIcon } from "./icons";
import { InstallCommand } from "./InstallCommand";

const RELEASE_URL = "https://github.com/smelt-ai/smelt/releases/latest";

export function Cta() {
  return (
    <section id="start" className="scroll-mt-24 px-6 py-24 sm:py-28">
      <div className="relative mx-auto max-w-6xl overflow-hidden rounded-[28px] border border-white/[0.08] bg-surface px-6 py-12 shadow-[inset_0_1px_0_rgba(255,255,255,0.08)] sm:px-10 sm:py-14">
        <div
          aria-hidden="true"
          className="pointer-events-none absolute -top-28 left-1/2 h-72 w-[40rem] -translate-x-1/2 rounded-full bg-[radial-gradient(ellipse_at_center,rgba(224,159,90,0.24),transparent_68%)]"
        />
        <div className="relative">
          <p className="pill">
            <span className="h-1.5 w-1.5 rounded-full bg-accent" />
            开始使用
          </p>
          <h2 className="mt-5 max-w-2xl text-3xl font-semibold tracking-tight text-foreground sm:text-4xl">
            现在就把本机桌面、远程服务器和手机连成一体
          </h2>
          <p className="mt-4 max-w-2xl text-base leading-7 text-muted">
            在工位用 macOS 或 Windows 原生桌面深入编码，在 Linux 上让代码和环境留在服务器，离开工位就用手机审批、看进度、把任务继续往下推。
          </p>
          <div className="mt-8 flex flex-col gap-3 sm:flex-row sm:flex-wrap">
            <a href={RELEASE_URL} target="_blank" rel="noopener noreferrer" className="btn btn-primary">
              <AppleLogo className="h-4 w-4" />
              <span className="text-sm font-semibold">下载 macOS 版</span>
            </a>
            <a href={RELEASE_URL} target="_blank" rel="noopener noreferrer" className="btn btn-secondary">
              <DesktopIcon className="h-4 w-4" />
              <span className="text-sm font-semibold">下载 Windows 版</span>
            </a>
            <Link href="/docs#远程访问" className="btn btn-secondary">
              <PhoneIcon className="h-4 w-4" />
              <span className="text-sm font-semibold">iOS / Android 移动端</span>
            </Link>
            <Link href="/docs#快速开始" className="btn btn-secondary">
              <span className="text-sm font-semibold">快速上手文档</span>
            </Link>
          </div>
          <div className="mt-6">
            <InstallCommand />
          </div>
        </div>
      </div>
    </section>
  );
}
