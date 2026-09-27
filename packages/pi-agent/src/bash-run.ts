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
 * 长时间命令从一开始就由宿主创建。这里只观察最多 `promoteAfterMs`：
 * 结束了就带回输出，还在跑或这一轮被取消就交回任务 id。不在 Pi 里持有进程。
 */
export async function watchHostCommand(input: {
	promoteAfterMs?: number;
	signal?: AbortSignal;
	start: () => Promise<{ ok: boolean; id?: string; error?: string }>;
	snapshot: (id: string) => Promise<HostCommandSnapshot>;
}): Promise<ManagedBashResult> {
	const started = await input.start();
	if (!started.ok || !started.id) {
		return { promoted: false, exitCode: 1, output: started.error || "无法启动命令" };
	}
	const id = started.id;
	const deadline = Date.now() + (input.promoteAfterMs ?? AUTO_BACKGROUND_SECONDS * 1000);
	while (Date.now() < deadline) {
		if (input.signal?.aborted) return { promoted: true, id };
		const snapshot = await input.snapshot(id);
		if (!snapshot.ok) return { promoted: false, exitCode: 1, output: snapshot.error || "无法读取任务状态" };
		if (snapshot.status && snapshot.status !== "running") {
			return {
				promoted: false,
				exitCode: snapshot.exitCode ?? null,
				output: snapshot.output ?? "",
			};
		}
		await delay(50, input.signal);
		if (input.signal?.aborted) return { promoted: true, id };
	}
	return { promoted: true, id };
}

function delay(ms: number, signal?: AbortSignal): Promise<void> {
	return new Promise((resolve) => {
		const timer = setTimeout(resolve, ms);
		signal?.addEventListener("abort", () => {
			clearTimeout(timer);
			resolve();
		}, { once: true });
	});
}
