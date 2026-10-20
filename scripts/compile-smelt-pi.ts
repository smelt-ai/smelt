/**
 * 把 packages/pi-agent 编成 smelt-pi。
 * bun 1.4.0 不会把 ./src/...worker.ts 登记成 Worker 能打开的路径，
 * 嵌入文件落在 /$bunfs 下。这里把 Pi 写死的两条说明符改成那两个路径。
 *
 * --splitting 会让 photon 的 wasm 绑定在 worker 里变成
 * `exports_photon_rs is not defined`，所以不用。
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

const IMAGE_WORKER = "/$bunfs/root/utils/image-resize-worker.js";
const CODEMODE_WORKER = "/$bunfs/root/extensions/codemode/worker.js";

const workerPaths: BunPlugin = {
	name: "smelt-pi-worker-paths",
	setup(build) {
		build.onLoad({ filter: /image-resize\.js$/ }, async (args) => {
			if (!args.path.includes("pi-coding-agent/dist/utils/image-resize.js")) return;
			const before = await Bun.file(args.path).text();
			const needle = '"./src/utils/image-resize-worker.ts"';
			if (!before.includes(needle)) {
				throw new Error(`image-resize.js 里没有 ${needle}`);
			}
			return {
				contents: before.replaceAll(needle, JSON.stringify(IMAGE_WORKER)),
				loader: "js",
			};
		});
		build.onLoad({ filter: /config\.js$/ }, async (args) => {
			if (!args.path.endsWith("pi-coding-agent/dist/config.js")) return;
			const before = await Bun.file(args.path).text();
			const needle = '"./src/extensions/codemode/worker.ts"';
			if (!before.includes(needle)) {
				throw new Error(`config.js 里没有 ${needle}`);
			}
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
	plugins: [workerPaths],
	compile: {
		outfile,
		target: compileTarget,
		autoloadDotenv: false,
		autoloadBunfig: false,
	},
});

if (!result.success) {
	for (const log of result.logs) console.error(log);
	process.exit(1);
}
