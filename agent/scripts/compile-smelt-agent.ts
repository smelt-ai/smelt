/**
 * 把 agent/ 编成 smelt-agent。
 * Bun 1.4.3 仍然打不开 `./src/...worker.ts`（Cannot find module）。
 * 嵌入文件落在 Bun 的虚拟文件系统里：macOS / Linux 是 /$bunfs/root/，Windows 是
 * B:/~BUN/root/。这里按编译目标把 Pi 写死的两条说明符改成对应平台上的那两个路径。
 * Pi 在这条路径失败时会退回进程内缩放，冒烟就看不出 worker 没起来。
 * 编进来的 MCP 服务器里按自身路径读文件的地方同理，见 `bunfsPaths`。
 *
 * --splitting 和图片 worker、codemode worker 一起要过冒烟（compiled-smoke.ts）。
 * Bun 1.4.0 上拆块会让 photon 报 `exports_photon_rs is not defined`，1.4.3 没有。
 *
 * 用法：bun agent/scripts/compile-smelt-agent.ts <outfile> <bun-target>
 */
import { dirname, join } from "node:path";

const outfile = process.argv[2];
const compileTarget = process.argv[3];
if (!outfile || !compileTarget) {
	console.error("用法：bun agent/scripts/compile-smelt-agent.ts <outfile> <bun-target>");
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

/** 编进来的代码按自身路径找文件的地方。编译后这些路径都落进 bunfs，文件不在那里。 */
const bunfsPaths: BunPlugin = {
	name: "smelt-agent-bunfs-paths",
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
		// Agent360 Browser MCP 启动时从自己旁边读 package.json 取版本号，bunfs 里没有这个文件。
		// 换成编译时读到的内容，读法和结果不变。
		build.onLoad({ filter: /index\.js$/ }, async (args) => {
			if (!posixPath(args.path).endsWith("@agent360/browser-mcp/index.js")) return;
			const before = await Bun.file(args.path).text();
			const needle = "readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'package.json'), 'utf8')";
			if (!before.includes(needle)) {
				throw new Error(`@agent360/browser-mcp/index.js 里没有 ${needle}`);
			}
			const manifest = await Bun.file(join(dirname(args.path), "package.json")).text();
			rewritten.add("@agent360/browser-mcp/index.js");
			return {
				contents: before.replaceAll(needle, JSON.stringify(manifest)),
				loader: "js",
			};
		});
	},
};

const root = join(import.meta.dir, "..");

/**
 * `compiled-main.ts` 用 `smelt-bin:<包名>` 引入 npm 包的命令行入口（ACP 适配器、MCP 服务器），这里按包
 * 自己在 package.json 里声明的 `bin` 找到那个文件。`bin` 和 `./package.json` 都是包对外的约定；
 * `dist/` 下的路径不是——claude-agent-acp 0.103 起 exports 就不再放行它。
 */
const PACKAGE_BIN = "smelt-bin:";
const packageBins: BunPlugin = {
	name: "smelt-package-bins",
	setup(build) {
		build.onResolve({ filter: /^smelt-bin:/ }, async (args) => {
			const name = args.path.slice(PACKAGE_BIN.length);
			const manifestPath = Bun.resolveSync(`${name}/package.json`, root);
			const manifest = await Bun.file(manifestPath).json();
			// 单个入口可以直接写成字符串；写成表时取和包名（去掉作用域）同名的那条，表里只有一条时就是它
			// （`@playwright/mcp` 的叫 playwright-mcp）。
			const command = name.slice(name.lastIndexOf("/") + 1);
			const bins: unknown[] = manifest.bin && typeof manifest.bin === "object" ? Object.values(manifest.bin) : [];
			const entry: unknown =
				typeof manifest.bin === "string"
					? manifest.bin
					: (manifest.bin?.[command] ?? (bins.length === 1 ? bins[0] : undefined));
			if (typeof entry !== "string") {
				throw new Error(`${name} 的 package.json 没有声明同名或唯一的 bin`);
			}
			return { path: join(dirname(manifestPath), entry) };
		});
	},
};

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
	plugins: [bunfsPaths, packageBins],
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
if (rewritten.size !== 3) {
	console.error(`bunfs 路径只改写了 ${[...rewritten].join("、") || "零处"}：Pi 或 MCP 服务器的文件路径变了？`);
	process.exit(1);
}
