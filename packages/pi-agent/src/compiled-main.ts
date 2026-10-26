import "./quickjs-embed.ts";
import pkg from "../package.json" with { type: "json" };
import { checksDaemonProtocol, parseCompiledCommand, protocolMismatch } from "./compiled-command.ts";

// 和 scripts/compile-smelt-pi.ts 里改写进 Pi 的路径一致：嵌入文件的根目录分平台。
const BUNFS_ROOT = process.platform === "win32" ? "B:/~BUN/root/" : "/$bunfs/root/";
const IMAGE_WORKER = `${BUNFS_ROOT}utils/image-resize-worker.js`;
const CODEMODE_WORKER = `${BUNFS_ROOT}extensions/codemode/worker.js`;

// 只给编译冒烟用。正常启动不设置这个变量。
if (process.env.SMELT_PI_PROBE_WORKERS === "1") {
	const { Worker } = await import("node:worker_threads");
	const png = new Uint8Array(
		Buffer.from(
			"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
			"base64",
		),
	);

	/** 等 worker 的第一份有效回包；出错或提前退出立即失败，最多等 10 秒。 */
	function reply(worker: InstanceType<typeof Worker>, specifier: string, accept: (value: unknown) => boolean) {
		return new Promise<unknown>((resolve, reject) => {
			const timer = setTimeout(() => reject(new Error(`${specifier} 没有回包`)), 10_000);
			worker.on("message", (value) => {
				if (!accept(value)) return;
				clearTimeout(timer);
				resolve(value);
			});
			worker.once("error", (error) => {
				clearTimeout(timer);
				reject(error);
			});
			worker.once("exit", (code) => {
				clearTimeout(timer);
				reject(new Error(`${specifier} 提前退出（${code}）`));
			});
		});
	}

	async function probe(specifier: string, message?: Uint8Array): Promise<void> {
		// 没有缩放请求的 worker（codemode）带上 smeltProbe：包装层在依赖全部求值后回一声。
		const worker = new Worker(specifier, message ? undefined : { workerData: { smeltProbe: true } });
		try {
			if (message) {
				const replied = reply(worker, specifier, () => true);
				const bytes = new Uint8Array(message);
				worker.postMessage({ inputBytes: bytes, mimeType: "image/png" }, [bytes.buffer]);
				const response = await replied;
				const record = response as { error?: string; result?: { width?: number } };
				if (record.error) throw new Error(record.error);
				if (!record.result?.width) {
					throw new Error(`${specifier} 没有返回缩放结果：${JSON.stringify(response)}`);
				}
			} else {
				await reply(
					worker,
					specifier,
					(value) => (value as { smeltProbe?: unknown } | null)?.smeltProbe === "ready",
				);
			}
			console.log(`worker ${specifier} ok`);
		} finally {
			await worker.terminate();
		}
	}

	try {
		await probe(IMAGE_WORKER, png);
		await probe(CODEMODE_WORKER);
	} catch (error) {
		console.error(error instanceof Error ? error.message : String(error));
		process.exit(1);
	}
	process.exit(0);
}

const command = parseCompiledCommand(process.argv);

const mismatch = checksDaemonProtocol(command) ? protocolMismatch(pkg.smeltProtocol, process.env) : undefined;
if (mismatch) {
	console.error(mismatch);
	process.exit(78);
}

function presentAs(rest: string[]): void {
	// auth-main / mcp-main 用 process.argv.slice(2)，所以 argv 要摆成
	// [argv0, 虚拟脚本, ...用户参数]。编译器插进来的 /$bunfs 路径已经丢掉。
	process.argv = [process.argv[0] ?? "smelt-pi", "smelt-pi", ...rest];
}

switch (command.kind) {
	case "auth":
		presentAs(command.args);
		await import("./auth-main.ts");
		break;
	case "mcp":
		presentAs(command.args);
		await import("./mcp-main.ts");
		break;
	case "gateway":
		await import("./mcp-gateway-main.ts");
		break;
	case "rpc":
		await import("./main.ts");
		break;
}
