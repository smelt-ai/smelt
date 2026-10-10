import { lstatSync, realpathSync } from "node:fs";
import type { MemoryRoots } from "./memory-files.ts";

/** 与 Rust `agent_kind::SMELT_AGENT_MEMORY_DIR_ENV` / `SMELT_USER_MEMORY_DIR_ENV` 保持一致。 */
export const SMELT_AGENT_MEMORY_DIR_ENV = "SMELT_AGENT_MEMORY_DIR";
export const SMELT_USER_MEMORY_DIR_ENV = "SMELT_USER_MEMORY_DIR";

let roots: MemoryRoots | undefined;

/** 本进程的记忆目录；普通 Pi 会话和子智能体没有。 */
export function currentMemoryRoots(): MemoryRoots | undefined {
	return roots;
}

export function resetMemoryRootsForTests(): void {
	roots = undefined;
}

/** 记忆根本身不能是符号链接；返回规范化后的真实路径，后续写入校验都以它为准。 */
function canonicalRoot(path: string | undefined): string | undefined {
	if (!path) return undefined;
	try {
		const stat = lstatSync(path);
		if (stat.isSymbolicLink() || !stat.isDirectory()) return undefined;
		return realpathSync(path);
	} catch {
		return undefined;
	}
}

/**
 * 在 Pi 启动前消费宿主下发的记忆目录。
 *
 * 环境变量读完立刻删除，子智能体和工具子进程都拿不到记忆。注入由 smelt-memory 扩展在
 * 会话里完成：它知道当前工作目录（按项目过滤事实类记忆），以后也能在缓存反正失效的时机
 * 重建记忆段落，这些启动参数都做不到。
 */
export function consumeAgentMemory(environment: Record<string, string | undefined>): boolean {
	const agentDir = environment[SMELT_AGENT_MEMORY_DIR_ENV]?.trim();
	const userDir = environment[SMELT_USER_MEMORY_DIR_ENV]?.trim();
	delete environment[SMELT_AGENT_MEMORY_DIR_ENV];
	delete environment[SMELT_USER_MEMORY_DIR_ENV];
	const agent = canonicalRoot(agentDir);
	const user = canonicalRoot(userDir);
	if (!agent || !user) return false;
	roots = { agent, user };
	return true;
}
