/**
 * Smelt 会话控制变量：只给 Pi 进程和它自己的 memory-host 用。
 * bash / 后台任务 / MCP server 拿到它们就能冒充这条会话调 smeltd（含
 * `agent.memory.record_write` / `approve`），所以子孙进程一个都不能继承。
 * 与 `mcpServerEnvironment` 同一套剥离规则；`SMELT_HOME` 只是数据目录，留着。
 */
export const SMELT_SESSION_CONTROL_ENV_NAMES = [
	"SMELT_SESSION_ID",
	"SMELT_AGENT_TOKEN",
	"SMELT_SOCK",
	"SMELT_BACKGROUND_TASK_SOCK",
] as const;

/** 从一份环境里去掉会话控制变量；其它 `SMELT_*`（除 `SMELT_HOME`）一并去掉。 */
export function withoutSessionControlEnv(
	environment: NodeJS.ProcessEnv | Record<string, string | undefined>,
): Record<string, string> {
	const result: Record<string, string> = {};
	for (const [name, value] of Object.entries(environment)) {
		if (value === undefined) continue;
		if (name.startsWith("SMELT_") && name !== "SMELT_HOME") continue;
		result[name] = value;
	}
	return result;
}

/** bash `spawnHook`：在启动命令前剥掉控制凭据。 */
export function stripSessionControlFromSpawnEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
	return withoutSessionControlEnv(env);
}
