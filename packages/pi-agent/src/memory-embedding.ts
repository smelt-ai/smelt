import { createHash } from "node:crypto";
import type { MemoryEntry } from "./memory-files.ts";

/**
 * 向量检索：用户在设置里手动配的 OpenAI 兼容 `/embeddings` 服务（含 Ollama 等本地服务）。
 * 没配、或服务不可用，调用方直接退回关键词 + 模型重排。
 *
 * 缓存：向量按「服务地址 + 模型 + 文本」的哈希缓存在 Pi 进程内存里。记忆改了只重算那一条；
 * 换了模型或服务，键就不同，旧向量不会被命中，也不会和新向量混着比。进程结束即失效。
 *
 * 单条失败不连累整批：很多向量模型有输入长度上限（如 bge 系列 512 token）。整批请求被拒
 * （4xx，内容问题）时拆成单条重试，算不出来的那几条记下来跳过，同一进程里不再重试；
 * 服务本身不可用（连不上、超时、5xx、鉴权失败）则整体失败，由调用方降级。
 */
export type EmbeddingConfig = { base_url: string; model: string; api_key?: string };

const EMBEDDING_TIMEOUT_MS = 3_000;
/** 一次请求最多带这么多条，避免单个请求过大。 */
const EMBEDDING_BATCH = 64;
/**
 * 送去向量化的文本上限。描述放在最前面，截断只丢正文后半；需要细节时模型重排和 read 仍用全文。
 * 1000 字约合 500–1000 token，多数向量模型的上限内。
 */
export const EMBEDDING_TEXT_MAX_CHARS = 1_000;

const cache = new Map<string, Float32Array>();
/** 这个进程里算不出来的文本（多半太长），不再重试。 */
const unembeddable = new Set<string>();

function cacheKey(config: EmbeddingConfig, text: string): string {
	return createHash("sha256").update(`${config.base_url}\n${config.model}\n${text}`).digest("hex");
}

export function resetEmbeddingCacheForTests(): void {
	cache.clear();
	unembeddable.clear();
}

export type EmbeddingFetch = typeof fetch;

/** 请求被拒的原因是不是内容本身（可以拆开逐条重试），而不是服务、鉴权或限流问题。 */
class EmbeddingRequestError extends Error {
	constructor(
		message: string,
		readonly contentRejected: boolean,
	) {
		super(message);
	}
}

const CONTENT_REJECTED_STATUSES = new Set([400, 413, 422]);

async function requestEmbeddings(
	config: EmbeddingConfig,
	texts: readonly string[],
	fetchImpl: EmbeddingFetch,
): Promise<Float32Array[]> {
	const url = `${config.base_url.replace(/\/+$/, "")}/embeddings`;
	const response = await fetchImpl(url, {
		method: "POST",
		headers: {
			"content-type": "application/json",
			...(config.api_key ? { authorization: `Bearer ${config.api_key}` } : {}),
		},
		body: JSON.stringify({ model: config.model, input: texts }),
		signal: AbortSignal.timeout(EMBEDDING_TIMEOUT_MS),
	});
	if (!response.ok) {
		throw new EmbeddingRequestError(
			`向量服务返回 ${response.status}`,
			CONTENT_REJECTED_STATUSES.has(response.status),
		);
	}
	const body = (await response.json()) as { data?: Array<{ index?: number; embedding?: number[] }> };
	const data = body.data ?? [];
	if (data.length !== texts.length) throw new EmbeddingRequestError("向量服务返回的条数不对", false);
	const ordered = [...data].sort((a, b) => (a.index ?? 0) - (b.index ?? 0));
	return ordered.map((item) => {
		if (!Array.isArray(item.embedding) || item.embedding.length === 0) {
			throw new EmbeddingRequestError("向量服务返回了空向量", false);
		}
		return Float32Array.from(item.embedding);
	});
}

/**
 * 取一批文本的向量，命中缓存的不重算。算不出来的那几条返回 undefined；
 * 服务本身不可用时抛出，由调用方降级。
 */
export async function embedTexts(
	config: EmbeddingConfig,
	texts: readonly string[],
	fetchImpl: EmbeddingFetch = fetch,
): Promise<Array<Float32Array | undefined>> {
	const keys = texts.map((text) => cacheKey(config, text));
	const missing = [
		...new Set(
			keys
				.map((key, index) => (cache.has(key) || unembeddable.has(key) ? -1 : index))
				.filter((index) => index >= 0),
		),
	];
	for (let start = 0; start < missing.length; start += EMBEDDING_BATCH) {
		const batch = missing.slice(start, start + EMBEDDING_BATCH);
		try {
			const vectors = await requestEmbeddings(config, batch.map((index) => texts[index]!), fetchImpl);
			batch.forEach((index, offset) => cache.set(keys[index]!, vectors[offset]!));
		} catch (error) {
			// 服务不可用、鉴权失败、限流：整体失败，交给调用方降级。
			if (!(error instanceof EmbeddingRequestError && error.contentRejected)) throw error;
			if (batch.length === 1) {
				unembeddable.add(keys[batch[0]!]!);
				continue;
			}
			// 整批被拒：拆成单条，找出是哪几条算不出来。
			for (const index of batch) {
				try {
					const [vector] = await requestEmbeddings(config, [texts[index]!], fetchImpl);
					cache.set(keys[index]!, vector!);
				} catch (single) {
					if (!(single instanceof EmbeddingRequestError && single.contentRejected)) throw single;
					unembeddable.add(keys[index]!);
				}
			}
		}
	}
	return keys.map((key) => cache.get(key));
}

export function cosine(a: Float32Array, b: Float32Array): number {
	if (a.length !== b.length) return 0;
	let dot = 0;
	let normA = 0;
	let normB = 0;
	for (let i = 0; i < a.length; i++) {
		dot += a[i]! * b[i]!;
		normA += a[i]! * a[i]!;
		normB += b[i]! * b[i]!;
	}
	return normA > 0 && normB > 0 ? dot / Math.sqrt(normA * normB) : 0;
}

/**
 * 记忆参与向量化的文本：名字、描述在前（Anthropic Contextual Retrieval 的思路：给正文带上
 * 它是什么），超过上限截断，只丢正文后半。
 */
export function embeddingText(entry: MemoryEntry): string {
	const text = `${entry.name.replaceAll("-", " ")}：${entry.description}\n${entry.body}`;
	const chars = [...text];
	return chars.length > EMBEDDING_TEXT_MAX_CHARS ? chars.slice(0, EMBEDDING_TEXT_MAX_CHARS).join("") : text;
}

export type VectorHit = { entry: MemoryEntry; similarity: number };

/** 向量检索结果；`skipped` 是算不出向量、这次没参与的记忆。 */
export type VectorSearchResult = { hits: VectorHit[]; skipped: MemoryEntry[] };

export async function vectorSearch(
	config: EmbeddingConfig,
	query: string,
	entries: readonly MemoryEntry[],
	limit: number,
	fetchImpl: EmbeddingFetch = fetch,
): Promise<VectorSearchResult> {
	if (entries.length === 0) return { hits: [], skipped: [] };
	const [queryVector, ...docVectors] = await embedTexts(config, [query, ...entries.map(embeddingText)], fetchImpl);
	if (!queryVector) throw new Error("问题本身算不出向量");
	const hits: VectorHit[] = [];
	const skipped: MemoryEntry[] = [];
	entries.forEach((entry, index) => {
		const vector = docVectors[index];
		if (vector) hits.push({ entry, similarity: cosine(queryVector, vector) });
		else skipped.push(entry);
	});
	return { hits: hits.sort((a, b) => b.similarity - a.similarity).slice(0, limit), skipped };
}
