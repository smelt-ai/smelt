#!/usr/bin/env bun

// Keep one Smelt-owned entrypoint while using Pi's public SDK entry. The permission factory is
// appended after discovered extensions, so host approval sees the final tool arguments.
import { registerBunOAuthFlows } from "@earendil-works/pi-ai/bun-oauth";
import { main } from "@earendil-works/pi-coding-agent";
import { consumeAgentInstructions } from "./agent-instructions.ts";
import { consumePluginArgs } from "./plugin-args.ts";
import { SMELT_EXTENSION_FACTORIES } from "./runtime-extensions.ts";

// 与 Pi 官方 bun CLI / auth-main 一样，启动时静态注册 OAuth flow。
// 否则 token 刷新会按 import.meta.url 再读磁盘；旧运行时目录被回收后就会 ENOENT。
registerBunOAuthFlows();
consumeAgentInstructions(process.env, process.argv);
consumePluginArgs(process.env, process.argv);
process.title = "pi-rpc";
process.env.PI_CODING_AGENT = "true";
process.env.AI_AGENT = "pi";
process.emitWarning = (() => {}) as typeof process.emitWarning;
await main(["--mode", "rpc", ...process.argv.slice(2)], {
	extensionFactories: SMELT_EXTENSION_FACTORIES,
});
