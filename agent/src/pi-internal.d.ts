// Pi 的发布包没有把这些内部文件放进 exports。编译必须用相对路径静态 import，
// 类型在这里补上，避免去磁盘上再解析一份 node_modules。
declare module "quickjs-wasi/quickjs.wasm" {
	const path: string;
	export default path;
}

declare module "../node_modules/@earendil-works/pi-coding-agent/dist/config.js" {
	export function setEmbeddedQuickJSWasmPath(path: string): void;
}

declare module "../node_modules/@earendil-works/pi-coding-agent/dist/extensions/mcp/runtime.js" {
	import type { McpTransportFactory } from "@earendil-works/pi-coding-agent";
	export const createDefaultTransport: McpTransportFactory;
}

declare module "../../node_modules/@earendil-works/pi-coding-agent/dist/utils/image-resize-worker.js";

declare module "../../../node_modules/@earendil-works/pi-coding-agent/dist/extensions/codemode/worker.js";
