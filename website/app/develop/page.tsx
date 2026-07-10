import { readFileSync } from "node:fs";
import path from "node:path";
import type { Metadata } from "next";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeSlug from "rehype-slug";
import { Nav } from "../components/Nav";
import { Footer } from "../components/Footer";
import { mdComponents } from "../components/mdComponents";

export const metadata: Metadata = {
  title: "开发 — smelt",
  description: "smelt 开发文档：从源码构建、二进制与架构、打包发布、目录结构。",
};

export default function DevelopPage() {
  const filePath = path.join(process.cwd(), "content", "develop.md");
  const source = readFileSync(filePath, "utf-8");

  return (
    <div className="flex flex-1 flex-col bg-background">
      <Nav />
      <main className="mx-auto flex w-full max-w-3xl flex-1 px-6 py-16">
        <article className="min-w-0 flex-1">
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            rehypePlugins={[rehypeSlug]}
            components={mdComponents}
          >
            {source}
          </ReactMarkdown>
        </article>
      </main>
      <Footer />
    </div>
  );
}
