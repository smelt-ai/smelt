// Pi 的 bun 发布包用 ./src/utils/image-resize-worker.ts 找 worker。
// bun 1.4.0 编出来的文件在 /$bunfs/root/utils/image-resize-worker.js，
// 编译插件把那条说明符改过去。这层必须留住自己的语句，否则入口会被折掉。
import { parentPort } from "node:worker_threads";
import "../../node_modules/@earendil-works/pi-coding-agent/dist/utils/image-resize-worker.js";

if (!parentPort) {
	throw new Error("smelt-pi image resize worker requires parentPort");
}
