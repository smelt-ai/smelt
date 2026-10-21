// Pi 写死的说明符是 ./src/extensions/codemode/worker.ts。
// quickjs-embed 必须先求值：ESM 按 import 顺序跑完依赖，再加载 dist worker。
// 这层必须留住自己的语句，否则 bun compile 会把入口折成 dist 的 bunfs 路径。
import { parentPort } from "node:worker_threads";
import "../../quickjs-embed.ts";
import "../../../node_modules/@earendil-works/pi-coding-agent/dist/extensions/codemode/worker.js";

if (!parentPort) {
	throw new Error("smelt-pi codemode worker requires parentPort");
}
