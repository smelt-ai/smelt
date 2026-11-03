import { AUTO_BACKGROUND_SECONDS } from "./background-task.ts";

export type ForegroundBash = {
	promoted: false;
	exitCode: number | null;
	output: string;
};

export type PromotedBash = {
	promoted: true;
	id: string;
};

export type ManagedBashResult = ForegroundBash | PromotedBash;

export type HostCommandSnapshot = {
	ok: boolean;
	status?: string;
	exitCode?: number | null;
	output?: string;
	error?: string;
};

/** 用户给了不超过自动转后台阈值的 timeout 时，按 Pi 的原意到点杀掉。 */
export function keepsForegroundTimeout(timeoutSeconds: number | undefined): boolean {
	return timeoutSeconds !== undefined && timeoutSeconds <= AUTO_BACKGROUND_SECONDS;
}

/**
 * 长时间命令从一开始就由宿主创建。这里只守到 `promoteAfterMs`：
 * 结束了就带回输出，还在跑、读失败或这一轮被取消就交回任务 id。不在 Pi 里持有进程。
 * `release` 告诉宿主工具有没有拿走结果：拿走了就不再通知，没拿走则由宿主补一次。
 */
export async function watchHostCommand(input: {
	promoteAfterMs?: number;
	signal?: AbortSignal;
	start: () => Promise<{ ok: boolean; id?: string; error?: string }>;
	until: (id: string, timeoutMs: number) => Promise<HostCommandSnapshot>;
	release?: (id: string, consumed: boolean) => Promise<void>;
}): Promise<ManagedBashResult> {
	const started = await input.start();
	if (!started.ok || !started.id) {
		return { promoted: false, exitCode: 1, output: started.error || "无法启动命令" };
	}
	const id = started.id;
	let consumed = false;
	try {
		if (input.signal?.aborted) return { promoted: true, id };
		const timeoutMs = Math.max(0, input.promoteAfterMs ?? AUTO_BACKGROUND_SECONDS * 1000);
		const snapshot = await input.until(id, timeoutMs);
		// 等的过程中被取消，或没读到输出。交给宿主补一次。
		if (input.signal?.aborted || !snapshot.ok) return { promoted: true, id };
		if (snapshot.status && snapshot.status !== "running") {
			consumed = true;
			return {
				promoted: false,
				exitCode: snapshot.exitCode ?? null,
				output: snapshot.output ?? "",
			};
		}
		return { promoted: true, id };
	} finally {
		if (input.release) {
			try {
				await input.release(id, consumed);
			} catch {
				// 释放失败时宿主会在监听断开后补发，不挡住已经拿到的工具结果。
			}
		}
	}
}
