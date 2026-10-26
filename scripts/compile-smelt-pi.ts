/**
 * 把 packages/pi-agent 编成 smelt-pi。
 * Bun 1.4.3 仍然打不开 `./src/...worker.ts`（Cannot find module）。
 * 嵌入文件落在 Bun 的虚拟文件系统里：macOS / Linux 是 /$bunfs/root/，Windows 是
 * B:/~BUN/root/。这里按编译目标把 Pi 写死的两条说明符改成对应平台上的那两个路径。
 * Pi 在这条路径失败时会退回进程内缩放，冒烟就看不出 worker 没起来。
 *
 * --splitting 在 Bun 1.4.3 上和图片 worker、codemode worker 一起通过了冒烟。
 * Bun 1.4.0 上拆块会让 photon 报 `exports_photon_rs is not defined`，1.4.3 没有。
 *
 * 用法：bun scripts/compile-smelt-pi.ts <outfile> <bun-target>
 */
import { join } from "node:path";

const outfile = process.argv[2];
const compileTarget = process.argv[3];
if (!outfile || !compileTarget) {
	console.error("用法：bun scripts/compile-smelt-pi.ts <outfile> <bun-target>");
	process.exit(2);
}

const BUNFS_ROOT = compileTarget.startsWith("bun-windows-") ? "B:/~BUN/root/" : "/$bunfs/root/";
const IMAGE_WORKER = `${BUNFS_ROOT}utils/image-resize-worker.js`;
const CODEMODE_WORKER = `${BUNFS_ROOT}extensions/codemode/worker.js`;

/** Windows 上 `args.path` 用反斜杠，统一成正斜杠再比。 */
function posixPath(path: string): string {
	return path.replaceAll("\\", "/");
}

/** 真改写过的文件。插件按路径挑文件，挑漏了构建照样成功，所以编完要核对。 */
const rewritten = new Set<string>();

const workerPaths: BunPlugin = {
	name: "smelt-pi-worker-paths",
	setup(build) {
		build.onLoad({ filter: /image-resize\.js$/ }, async (args) => {
			if (!posixPath(args.path).includes("pi-coding-agent/dist/utils/image-resize.js")) return;
			const before = await Bun.file(args.path).text();
			const needle = '"./src/utils/image-resize-worker.ts"';
			if (!before.includes(needle)) {
				throw new Error(`image-resize.js 里没有 ${needle}`);
			}
			rewritten.add("image-resize.js");
			return {
				contents: before.replaceAll(needle, JSON.stringify(IMAGE_WORKER)),
				loader: "js",
			};
		});
		build.onLoad({ filter: /config\.js$/ }, async (args) => {
			if (!posixPath(args.path).endsWith("pi-coding-agent/dist/config.js")) return;
			const before = await Bun.file(args.path).text();
			const needle = '"./src/extensions/codemode/worker.ts"';
			if (!before.includes(needle)) {
				throw new Error(`config.js 里没有 ${needle}`);
			}
			rewritten.add("config.js");
			return {
				contents: before.replaceAll(needle, JSON.stringify(CODEMODE_WORKER)),
				loader: "js",
			};
		});
	},
};

const root = join(import.meta.dir, "../packages/pi-agent");
const result = await Bun.build({
	entrypoints: [
		join(root, "src/compiled-main.ts"),
		join(root, "src/utils/image-resize-worker.ts"),
		join(root, "src/extensions/codemode/worker.ts"),
	],
	target: "bun",
	format: "esm",
	bytecode: true,
	splitting: true,
	plugins: [workerPaths],
	compile: {
		outfile,
		target: compileTarget,
		// 认运行那台机器的系统证书库（macOS 钥匙串、Linux /etc/ssl），和 Rust 侧
		// reqwest 的 native-tls 一致。只认 Bun 自带的根证书时，做 HTTPS 解密的公司
		// 网络里登录、调模型都会报 TLS 错误。
		execArgv: ["--use-system-ca"],
		autoloadDotenv: false,
		autoloadBunfig: false,
	},
});

if (!result.success) {
	for (const log of result.logs) console.error(log);
	process.exit(1);
}
if (rewritten.size !== 2) {
	console.error(`worker 说明符只改写了 ${[...rewritten].join("、") || "零处"}：Pi 的文件路径变了？`);
	process.exit(1);
}
