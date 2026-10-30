import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // 官网托管在 GitHub Pages：构建时导出成纯静态文件（out/），没有服务端。
  output: "export",
  // 项目站点挂在 smelt-ai.github.io/smelt 下。next/link 和静态导入的资源会自动带上前缀；
  // 写死成根路径的 src、普通 <a href> 不会，站内地址一律走这两种。
  basePath: "/smelt",
  // 静态导出没有图片优化服务，统一关掉，不靠每个 <Image> 各自写 unoptimized。
  images: { unoptimized: true },
};

export default nextConfig;
