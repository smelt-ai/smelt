/**
 * 列出编进 smelt-agent 的 npm 依赖及其许可证全文，写到标准输出。
 * 从 package.json 的 dependencies 出发，按 Node 的查找规则沿 node_modules 递归。
 * 只走 dependencies：optionalDependencies 里是各平台的原生包（如 Claude Code 二进制），
 * 不编进 smelt-agent；需要随包分发的原生文件（pi-tui 的 .node）属于 pi-tui 本身。
 * 用法：bun agent/scripts/third-party-licenses.ts（先 bun install）
 */
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";

const root = join(import.meta.dir, "..");

type Manifest = {
	name: string;
	version: string;
	license?: string | { type?: string };
	licenses?: { type?: string }[];
	repository?: string | { url?: string };
	dependencies?: Record<string, string>;
};

function readManifest(dir: string): Manifest {
	return JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
}

/** 从 fromDir 往上找 node_modules/<name>，到 agent/ 为止。 */
function resolve(name: string, fromDir: string): string {
	let dir = fromDir;
	for (;;) {
		const candidate = join(dir, "node_modules", name);
		if (existsSync(join(candidate, "package.json"))) return candidate;
		if (dir === root) break;
		const parent = dirname(dir);
		if (parent === dir) break;
		dir = parent;
	}
	throw new Error(`找不到 ${name}（从 ${fromDir} 起）；先在 agent/ 里 bun install`);
}

function licenseField(m: Manifest): string {
	if (typeof m.license === "string") return m.license;
	if (m.license?.type) return m.license.type;
	const list = m.licenses?.map((l) => l.type).filter(Boolean);
	return list?.length ? list.join(" OR ") : "UNKNOWN";
}

function repositoryField(m: Manifest): string | undefined {
	const repo = typeof m.repository === "string" ? m.repository : m.repository?.url;
	return repo?.replace(/^git\+/, "");
}

const LICENSE_FILE = /^(licen[cs]e|copying|notice)([.-].*)?$/i;

function licenseTexts(dir: string): string[] {
	return readdirSync(dir)
		.filter((f) => LICENSE_FILE.test(f))
		.sort()
		.map((f) => readFileSync(join(dir, f), "utf8").trimEnd());
}

const seen = new Map<string, string>();
const pending = [root];
while (pending.length > 0) {
	const dir = pending.pop()!;
	const manifest = readManifest(dir);
	for (const name of Object.keys(manifest.dependencies ?? {})) {
		const dep = resolve(name, dir);
		const m = readManifest(dep);
		const key = `${m.name}@${m.version}`;
		if (seen.has(key)) continue;
		seen.set(key, dep);
		pending.push(dep);
	}
}

/** 包里没带许可证文件时，按 license 字段附标准全文（每种只附一次，放在最后）。 */
const STANDARD_TEXTS: Record<string, () => string> = {
	MIT: () => `MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`,
	// 仓库根的 LICENSE 就是 Apache-2.0 原文。
	"Apache-2.0": () => readFileSync(join(root, "..", "LICENSE"), "utf8").trimEnd(),
};

const rule = "=".repeat(80);
const out: string[] = [];
const standardUsed = new Set<string>();
for (const key of [...seen.keys()].sort()) {
	const dir = seen.get(key)!;
	const m = readManifest(dir);
	const license = licenseField(m);
	out.push(rule, `${m.name} ${m.version}`, `License: ${license}`);
	const repo = repositoryField(m);
	if (repo) out.push(`Repository: ${repo}`);
	out.push("");
	const texts = licenseTexts(dir);
	if (texts.length > 0) {
		out.push(texts.join("\n\n"), "");
	} else if (STANDARD_TEXTS[license]) {
		standardUsed.add(license);
		out.push(`(No license file in the package; the standard ${license} text is at the end of this section.)`, "");
	} else {
		throw new Error(`${key} 没带许可证文件，license 字段 ${license} 也没有标准全文可附`);
	}
}
for (const license of [...standardUsed].sort()) {
	out.push(rule, `Standard ${license} text`, "", STANDARD_TEXTS[license](), "");
}
process.stdout.write(out.join("\n"));
