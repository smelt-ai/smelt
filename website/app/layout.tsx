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

const title = "Smelt — 跨机器、多智能体的原生 AI 编程指挥台";
const description =
  "The Multi-Agent, Multi-Machine AI Coding Cockpit. macOS GPUI 原生桌面端、Linux 无头守护进程 smeltd，以及 iOS/Android 手机与平板指挥台「出菜口」。在同一套工作台里指挥所有机器上的 AI 编程智能体。";

export const metadata: Metadata = {
  title: {
    default: title,
    template: "%s",
  },
  description,
  openGraph: {
    title: "Smelt — The Multi-Agent, Multi-Machine AI Coding Cockpit",
    description,
    siteName: "Smelt",
    locale: "zh_CN",
    type: "website",
  },
  twitter: {
    card: "summary",
    title: "Smelt — The Multi-Agent, Multi-Machine AI Coding Cockpit",
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
      className={`dark ${geistSans.variable} ${geistMono.variable} h-full antialiased`}
    >
      <body className="min-h-full flex flex-col">{children}</body>
    </html>
  );
}
