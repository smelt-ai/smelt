// 和 Pi 的 dist/bun/runtime-setup.js 一样：编译产物里静态收进 quickjs.wasm，
// 否则 codemode 会用 createRequire 到磁盘上找 quickjs-wasi。
import quickjsWasmPath from "quickjs-wasi/quickjs.wasm";
import { setEmbeddedQuickJSWasmPath } from "../node_modules/@earendil-works/pi-coding-agent/dist/config.js";

setEmbeddedQuickJSWasmPath(quickjsWasmPath);
