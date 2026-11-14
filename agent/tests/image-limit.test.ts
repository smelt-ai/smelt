import { describe, expect, test } from "bun:test";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { IMAGE_OMITTED_TEXT, imagesToOmit, limitImages } from "../src/image-limit.ts";

const image = { type: "image", data: "AAAA", mimeType: "image/png" };

function screenshot(i: number): AgentMessage {
	return {
		role: "toolResult",
		toolCallId: `c${i}`,
		toolName: "browser_screenshot",
		content: [{ type: "text", text: `shot ${i}` }, image],
		isError: false,
		timestamp: i,
	} as AgentMessage;
}

function countImages(messages: AgentMessage[]): number {
	return messages.reduce(
		(sum, m) => sum + (Array.isArray((m as any).content) ? (m as any).content.filter((b: any) => b.type === "image").length : 0),
		0,
	);
}

describe("image limit", () => {
	test("omits the oldest images in batches of 20", () => {
		expect(imagesToOmit(40)).toBe(0);
		expect(imagesToOmit(41)).toBe(20);
		expect(imagesToOmit(59)).toBe(20);
		expect(imagesToOmit(60)).toBe(40);
		// 保留量始终在 20～39，低于 OpenAI 系 50 张的上限
		for (let total = 41; total < 200; total += 1) {
			const kept = total - imagesToOmit(total);
			expect(kept).toBeGreaterThanOrEqual(20);
			expect(kept).toBeLessThan(40);
		}
	});

	test("leaves small contexts untouched", () => {
		const messages = Array.from({ length: 40 }, (_, i) => screenshot(i));
		expect(limitImages(messages)).toBeUndefined();
	});

	test("replaces the oldest images with a placeholder and keeps the newest", () => {
		// 卡死会话的现场：59 张图
		const messages: AgentMessage[] = [
			...Array.from({ length: 58 }, (_, i) => screenshot(i)),
			{ role: "user", content: [{ type: "text", text: "这个问题还没修复" }, image], timestamp: 99 } as AgentMessage,
		];
		const limited = limitImages(messages)!;
		expect(countImages(limited)).toBe(39);
		expect((limited[0] as any).content).toEqual([
			{ type: "text", text: "shot 0" },
			{ type: "text", text: IMAGE_OMITTED_TEXT },
		]);
		expect(limited[20]).toBe(messages[20]);
		expect(limited.at(-1)).toBe(messages.at(-1));
		// 原消息不被改动
		expect(countImages(messages)).toBe(59);
	});
});
