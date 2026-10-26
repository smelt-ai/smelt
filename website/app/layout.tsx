import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import "./globals.css";

const geistSans = Geist({
  variable: "--font-geist-sans",
  subsets: ["latin"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin"],
});

const title = "Smelt — 把所有机器上的 AI 编程智能体收进同一个原生工作台";
const description =
  "在原生 macOS 与 Windows 工作台里并行指挥 Claude Code、Codex 与内置智能体。一行命令直连 Linux 开发机；离开电脑后，用 iOS 与 Android 手机审批权限、查看 Diff、接管任务。";

export const metadata: Metadata = {
  title: {
    default: title,
    template: "%s",
  },
  description,
  openGraph: {
    title,
    description,
    siteName: "Smelt",
    locale: "zh_CN",
    type: "website",
  },
  twitter: {
    card: "summary",
    title,
    description,
  },
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html
      lang="zh-CN"
      data-scroll-behavior="smooth"
      className={`dark ${geistSans.variable} ${geistMono.variable} h-full antialiased`}
    >
      <body className="min-h-full flex flex-col">{children}</body>
    </html>
  );
}
