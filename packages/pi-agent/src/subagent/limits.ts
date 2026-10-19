/**
 * 子智能体的资源上限。每个子智能体都是一个完整的 Pi 进程，一次调用的并发上限挡不住多次后台调用
 * 叠加；轮数、stderr、进度更新不设上限时，一个跑偏的子进程就能拖垮父会话。
 */

/** 一个父 Pi 进程里同时在跑的子 Pi 进程上限。所有 subagent 调用共用，多出来的排队。 */
export const MAX_RUNNING_SUBAGENTS = 4;
/**
 * 要求收尾之后再给这么多轮；还在调工具就中止。轮数上限本身默认不设：写代码的子智能体跑上百次
 * 模型调用很正常，一刀切只会交回半成品。只有智能体定义里写了 `max_turns` 才限。
 */
export const SUBAGENT_WRAP_UP_TURNS = 5;
/** 子进程 stderr 只留最后这么多字符：出错时要看的是结尾。只在进程内用，不进父会话。 */
export const STDERR_TAIL_CHARS = 16_000;
/** 进度更新最密这么久一次。子智能体每调一次工具、每说完一句都有一份。 */
export const PROGRESS_INTERVAL_MS = 500;

export function subagentWrapUpMessage(maxTurns: number): string {
	return [
		`You have used ${maxTurns} turns, the limit for this subagent.`,
		"Stop exploring and do not call any more tools.",
		"Reply now with your final result based on what you already know, and say what is left undone.",
	].join(" ");
}

export function turnLimitMessage(turns: number, maxTurns: number): string {
	return `子智能体跑了 ${turns} 轮仍在调用工具，超过上限 ${maxTurns} 轮，已中止。`;
}

export type TurnLimitAction = "continue" | "wrap_up" | "abort";

/**
 * 一条助手消息结束后怎么办。`maxTurns` 为空（定义里没写）时不限；`continuing` 为假
 * （它这轮就收尾了）时什么也不用做。
 */
export function turnLimitAction(
	turns: number,
	continuing: boolean,
	maxTurns: number | undefined,
	wrapUpTurns = SUBAGENT_WRAP_UP_TURNS,
): TurnLimitAction {
	if (maxTurns === undefined || !continuing) return "continue";
	if (turns >= maxTurns + wrapUpTurns) return "abort";
	if (turns === maxTurns) return "wrap_up";
	return "continue";
}

/** 追加后只留结尾 `max` 个字符。 */
export function appendTail(text: string, chunk: string, max = STDERR_TAIL_CHARS): string {
	const next = text + chunk;
	return next.length <= max ? next : next.slice(next.length - max);
}

/**
 * 进程名额：拿到才能起子进程，用完交回，排队的按先来后到拿。
 * `acquire` 在排队时 `signal` 中止就不再占位，返回 undefined；交回函数只算第一次。
 */
export class ProcessSlots {
	private running = 0;
	private readonly waiting: Array<() => void> = [];

	constructor(readonly limit: number) {}

	get active(): number {
		return this.running;
	}

	get queued(): number {
		return this.waiting.length;
	}

	acquire(signal?: AbortSignal): Promise<(() => void) | undefined> {
		if (signal?.aborted) return Promise.resolve(undefined);
		if (this.running < this.limit) {
			this.running += 1;
			return Promise.resolve(this.releaser());
		}
		return new Promise((resolve) => {
			const grant = () => {
				signal?.removeEventListener("abort", cancel);
				this.running += 1;
				resolve(this.releaser());
			};
			const cancel = () => {
				const index = this.waiting.indexOf(grant);
				if (index >= 0) this.waiting.splice(index, 1);
				resolve(undefined);
			};
			this.waiting.push(grant);
			signal?.addEventListener("abort", cancel, { once: true });
		});
	}

	private releaser(): () => void {
		let released = false;
		return () => {
			if (released) return;
			released = true;
			this.running -= 1;
			this.waiting.shift()?.();
		};
	}
}

/**
 * 只送最新的一份，最密 `intervalMs` 一次：第一份立即送，间隔内来的合并成间隔结束时的一份。
 * `cancel` 丢掉还没送的那份，用在结果已经交出之后。
 */
export function throttleLatest<T>(
	deliver: (value: T) => void,
	intervalMs = PROGRESS_INTERVAL_MS,
): { push(value: T): void; cancel(): void } {
	let last = Number.NEGATIVE_INFINITY;
	let pending: { value: T } | undefined;
	let timer: ReturnType<typeof setTimeout> | undefined;
	const fire = () => {
		timer = undefined;
		if (!pending) return;
		const { value } = pending;
		pending = undefined;
		last = Date.now();
		deliver(value);
	};
	return {
		push(value) {
			pending = { value };
			if (timer) return;
			const wait = last + intervalMs - Date.now();
			if (wait <= 0) fire();
			else timer = setTimeout(fire, wait);
		},
		cancel() {
			pending = undefined;
			if (timer) clearTimeout(timer);
			timer = undefined;
		},
	};
}
