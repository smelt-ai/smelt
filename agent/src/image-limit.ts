import type { AgentMessage } from "@earendil-works/pi-agent-core";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

/**
 * 每次请求最多带多少张图。超过就把最老的一批图换成文字占位。
 *
 * 服务商对单次请求的图片数有硬上限（OpenAI 系 50 张，Anthropic 100 张），超了直接 400，
 * 而且不算超窗，Pi 不会压缩，对话之后每条都失败。浏览器截图多的对话很快就攒满。
 */
export const MAX_IMAGES = 40;
/** 每次裁掉的批量。按批裁而不是一张张裁，请求前缀每 20 张才变一次，提示缓存不会每轮失效。 */
export const IMAGE_DROP_STEP = 20;

export const IMAGE_OMITTED_TEXT = "[image omitted: older image removed to stay within the per-request image limit]";

/** 要换掉的最老图片张数：不超限为 0，超限时按 {@link IMAGE_DROP_STEP} 取整，保留 20～39 张。 */
export function imagesToOmit(total: number): number {
	if (total <= MAX_IMAGES) return 0;
	return Math.floor((total - IMAGE_DROP_STEP) / IMAGE_DROP_STEP) * IMAGE_DROP_STEP;
}

type ContentBlock = { type: string };

function imageBlocks(message: AgentMessage): number {
	if (message.role !== "user" && message.role !== "toolResult") return 0;
	const content = message.content as string | ContentBlock[];
	if (typeof content === "string") return 0;
	return content.filter((block) => block.type === "image").length;
}

/** 返回裁过图片的新消息列表；不需要裁时返回 undefined。不改动传入的消息。 */
export function limitImages(messages: AgentMessage[]): AgentMessage[] | undefined {
	const total = messages.reduce((sum, message) => sum + imageBlocks(message), 0);
	let remaining = imagesToOmit(total);
	if (remaining === 0) return undefined;
	return messages.map((message) => {
		if (remaining === 0 || imageBlocks(message) === 0) return message;
		const content = (message as { content: ContentBlock[] }).content.map((block) => {
			if (block.type !== "image" || remaining === 0) return block;
			remaining -= 1;
			return { type: "text", text: IMAGE_OMITTED_TEXT };
		});
		return { ...message, content } as AgentMessage;
	});
}

export default function smeltImageLimitExtension(pi: ExtensionAPI): void {
	pi.on("context", (event) => {
		const messages = limitImages(event.messages);
		if (messages) return { messages };
	});
}
