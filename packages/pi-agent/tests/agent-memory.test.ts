import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
	SMELT_AGENT_MEMORY_DIR_ENV,
	SMELT_USER_MEMORY_DIR_ENV,
	consumeAgentMemory,
	currentMemoryRoots,
	resetMemoryRootsForTests,
} from "../src/agent-memory.ts";
import {
	CORE_MEMORY_MAX_BYTES,
	FACT_INDEX_MAX_LINES,
	checkCoreBudget,
	recallableFacts,
	renderMemorySnapshot,
	resolveMemoryTarget,
	validateMemoryContent,
} from "../src/memory-files.ts";
import {
	cosine,
	EMBEDDING_TEXT_MAX_CHARS,
	embedTexts,
	embeddingText,
	resetEmbeddingCacheForTests,
	vectorSearch,
} from "../src/memory-embedding.ts";
import {
	findConfiguredModel,
	LexicalIndex,
	pickRecallModel,
	renderRecall,
	rerankCandidates,
	rerankWithModel,
	rrfFuse,
	tokenize,
} from "../src/memory-recall.ts";
import {
	fetchMemoryMeta,
	recordMemoryRead,
	recordMemoryWrite,
	setMemoryHostTransportForTests,
} from "../src/memory-host.ts";
import { FakeMemoryHost } from "./memory-host-fake.ts";

const temps: string[] = [];
afterEach(() => {
	for (const dir of temps.splice(0)) rmSync(dir, { recursive: true, force: true });
	resetMemoryRootsForTests();
});

function roots() {
	const base = realpathSync(mkdtempSync(join(tmpdir(), "smelt-memory-")));
	temps.push(base);
	const agent = join(base, "agents", "writer", "memory");
	const user = join(base, "memory");
	mkdirSync(agent, { recursive: true });
	mkdirSync(user, { recursive: true });
	return { base, agent, user };
}

function memoryFile(
	name: string,
	type: string,
	body = "回复一律用中文，代码注释保持英文。",
	extra = "",
	description = "用户要求中文回复",
) {
	return `---\nname: ${name}\ndescription: ${description}\ntype: ${type}\n${extra}---\n${body}\n`;
}

describe("consumeAgentMemory", () => {
	test("plain sessions get no memory", () => {
		expect(consumeAgentMemory({})).toBe(false);
		expect(currentMemoryRoots()).toBeUndefined();
	});

	test("consumes both variables so tool and subagent processes cannot inherit them", () => {
		const { agent, user } = roots();
		const env: Record<string, string | undefined> = {
			[SMELT_AGENT_MEMORY_DIR_ENV]: agent,
			[SMELT_USER_MEMORY_DIR_ENV]: user,
		};
		expect(consumeAgentMemory(env)).toBe(true);
		expect(env[SMELT_AGENT_MEMORY_DIR_ENV]).toBeUndefined();
		expect(env[SMELT_USER_MEMORY_DIR_ENV]).toBeUndefined();
		expect(currentMemoryRoots()).toEqual({ agent, user });
	});

	test("a half-configured launch or a symlinked root is ignored rather than guessed", () => {
		const { base, agent, user } = roots();
		const env: Record<string, string | undefined> = { [SMELT_AGENT_MEMORY_DIR_ENV]: agent };
		expect(consumeAgentMemory(env)).toBe(false);
		expect(env[SMELT_AGENT_MEMORY_DIR_ENV]).toBeUndefined();
		const linked = join(base, "linked");
		symlinkSync(agent, linked);
		expect(consumeAgentMemory({ [SMELT_AGENT_MEMORY_DIR_ENV]: linked, [SMELT_USER_MEMORY_DIR_ENV]: user })).toBe(
			false,
		);
	});
});

describe("renderMemorySnapshot", () => {
	test("an empty memory is only the short protocol", () => {
		const { agent, user } = roots();
		const snapshot = renderMemorySnapshot({ agent, user }, agent);
		expect(snapshot.truncated).toBe(false);
		expect(snapshot.text).not.toContain("### 规则");
		expect(snapshot.text).not.toContain("### 事实索引");
		expect(snapshot.text).toContain(agent);
		expect(snapshot.text).toContain(user);
		// 常驻协议要短：格式细节只在写错时由校验返回。
		expect(snapshot.text.replaceAll(agent, "").replaceAll(user, "").length).toBeLessThan(450);
	});

	test("rules are injected in full, facts only as one-line pointers", () => {
		const { agent, user } = roots();
		writeFileSync(join(user, "reply-in-chinese.md"), memoryFile("reply-in-chinese", "feedback"));
		writeFileSync(join(agent, "weekly-report.md"), memoryFile("weekly-report", "feedback", "周报先列风险再列进度。"));
		writeFileSync(
			join(agent, "release-flow.md"),
			memoryFile("release-flow", "project", "正文很长的发布流程细节", "", "发布走 fx-deploy 流水线"),
		);

		const { text } = renderMemorySnapshot({ agent, user }, agent);

		expect(text).toContain("- reply-in-chinese（全局）：回复一律用中文，代码注释保持英文。");
		expect(text).toContain("- weekly-report：周报先列风险再列进度。");
		expect(text).toContain("- release-flow — 发布走 fx-deploy 流水线");
		expect(text).not.toContain("正文很长的发布流程细节");
	});

	test("facts tied to another project are left out", () => {
		const { base, agent, user } = roots();
		const projectA = join(base, "work", "a");
		writeFileSync(
			join(agent, "a-fact.md"),
			memoryFile("a-fact", "project", "x", `project: ${projectA}\n`, "A 项目用 pnpm"),
		);
		writeFileSync(join(agent, "shared.md"), memoryFile("shared", "reference", "x", "", "需求看板在飞书"));

		expect(renderMemorySnapshot({ agent, user }, join(projectA, "src")).text).toContain("A 项目用 pnpm");
		const elsewhere = renderMemorySnapshot({ agent, user }, join(base, "work", "b")).text;
		expect(elsewhere).not.toContain("A 项目用 pnpm");
		expect(elsewhere).toContain("需求看板在飞书");
	});

	test("each layer has its own rule budget, so two full layers are both injected in full", () => {
		const { agent, user } = roots();
		const nearlyFull = "规".repeat(Math.floor(CORE_MEMORY_MAX_BYTES / 3) - 10);
		writeFileSync(join(user, "global-rule.md"), memoryFile("global-rule", "feedback", nearlyFull));
		writeFileSync(join(agent, "agent-rule.md"), memoryFile("agent-rule", "feedback", nearlyFull));
		expect(checkCoreBudget(user, "global-rule.md", memoryFile("global-rule", "feedback", nearlyFull))).toBeUndefined();
		expect(checkCoreBudget(agent, "agent-rule.md", memoryFile("agent-rule", "feedback", nearlyFull))).toBeUndefined();

		const snapshot = renderMemorySnapshot({ agent, user }, agent);

		expect(snapshot.truncated).toBe(false);
		expect(snapshot.text).toContain("- global-rule（全局）：");
		expect(snapshot.text).toContain("- agent-rule：");
	});

	test("hand-edited overflow is truncated newest first and reported", () => {
		const { agent, user } = roots();
		const body = "规".repeat(Math.ceil(CORE_MEMORY_MAX_BYTES / 3 / 2) + 10);
		writeFileSync(join(agent, "old.md"), memoryFile("old", "feedback", body));
		utimesSync(join(agent, "old.md"), new Date(1_000), new Date(1_000));
		writeFileSync(join(agent, "new.md"), memoryFile("new", "feedback", body));
		for (let i = 0; i < FACT_INDEX_MAX_LINES + 5; i++) {
			writeFileSync(join(agent, `fact-${i}.md`), memoryFile(`fact-${i}`, "reference", "x"));
		}

		const snapshot = renderMemorySnapshot({ agent, user }, agent);

		expect(snapshot.truncated).toBe(true);
		expect(snapshot.text).toContain("- new：");
		expect(snapshot.text).not.toContain("- old：");
		expect(snapshot.text.match(/- fact-\d+ — /g)).toHaveLength(FACT_INDEX_MAX_LINES);
	});

	test("broken files are not injected", () => {
		const { agent, user } = roots();
		writeFileSync(join(agent, "broken.md"), "手改坏了");
		expect(renderMemorySnapshot({ agent, user }, agent).text).not.toContain("broken");
	});
});

describe("resolveMemoryTarget", () => {
	test("accepts relative, absolute and @-prefixed paths inside a memory root", () => {
		const { agent, user } = roots();
		const memoryRoots = { agent, user };
		expect(resolveMemoryTarget(join(agent, "a.md"), "/elsewhere", memoryRoots)).toMatchObject({
			kind: "memory",
			layer: "agent",
			fileName: "a.md",
		});
		expect(resolveMemoryTarget("./b.md", user, memoryRoots)).toMatchObject({ kind: "memory", layer: "user" });
		expect(resolveMemoryTarget(`@${join(agent, "c.md")}`, "/", memoryRoots)).toMatchObject({ kind: "memory" });
	});

	test("ignores paths outside the memory roots", () => {
		const { base, agent, user } = roots();
		expect(resolveMemoryTarget(join(base, "notes.md"), "/", { agent, user })).toEqual({ kind: "outside" });
		expect(resolveMemoryTarget(join(agent, "..", "notes.md"), "/", { agent, user })).toEqual({ kind: "outside" });
	});

	test("rejects MEMORY.md, nested folders, odd names and symlinks inside a memory root", () => {
		const { base, agent, user } = roots();
		const memoryRoots = { agent, user };
		expect(resolveMemoryTarget(join(agent, "MEMORY.md"), "/", memoryRoots)).toMatchObject({
			kind: "invalid",
			reason: expect.stringContaining("自动生成"),
		});
		expect(resolveMemoryTarget(join(agent, "sub", "a.md"), "/", memoryRoots).kind).toBe("invalid");
		expect(resolveMemoryTarget(join(agent, "Bad Name.md"), "/", memoryRoots).kind).toBe("invalid");
		expect(resolveMemoryTarget(join(agent, "a.txt"), "/", memoryRoots).kind).toBe("invalid");
		writeFileSync(join(base, "outside.md"), "x");
		symlinkSync(join(base, "outside.md"), join(agent, "link.md"));
		expect(resolveMemoryTarget(join(agent, "link.md"), "/", memoryRoots).kind).toBe("invalid");
	});

	test("a symlinked memory root is never trusted", () => {
		const { base, user } = roots();
		const real = join(base, "real-memory");
		mkdirSync(real);
		const linked = join(base, "linked-memory");
		symlinkSync(real, linked);
		expect(resolveMemoryTarget(join(linked, "a.md"), "/", { agent: linked, user }).kind).toBe("invalid");
	});
});

describe("validateMemoryContent", () => {
	test("accepts a well-formed memory file", () => {
		expect(validateMemoryContent("agent", "reply-in-chinese.md", memoryFile("reply-in-chinese", "feedback"))).toBeUndefined();
	});

	test("format errors carry the format the prompt no longer spells out", () => {
		expect(validateMemoryContent("agent", "a.md", "随手记一笔")).toContain("type: user | feedback");
		expect(validateMemoryContent("agent", "a.md", memoryFile("b", "feedback"))).toContain("name");
		expect(validateMemoryContent("agent", "a.md", memoryFile("a", "diary"))).toContain("type");
		expect(validateMemoryContent("agent", "a.md", "---\nname: a\ntype: user\n---\n内容\n")).toContain("description");
		expect(validateMemoryContent("agent", "a.md", memoryFile("a", "user", ""))).toContain("正文");
	});

	test("descriptions stay one line long", () => {
		expect(
			validateMemoryContent("agent", "a.md", memoryFile("a", "user", "x", "", "长".repeat(81))),
		).toContain("80");
	});

	test("project must be an absolute path", () => {
		expect(validateMemoryContent("agent", "a.md", memoryFile("a", "project", "x", "project: work/a\n"))).toContain(
			"绝对路径",
		);
		expect(validateMemoryContent("agent", "a.md", memoryFile("a", "project", "x", "project: /work/a\n"))).toBeUndefined();
	});

	test("the user layer only holds user and feedback memories", () => {
		expect(validateMemoryContent("user", "a.md", memoryFile("a", "feedback"))).toBeUndefined();
		expect(validateMemoryContent("user", "a.md", memoryFile("a", "project"))).toContain("全局");
	});

	test("caps a single memory file", () => {
		expect(validateMemoryContent("agent", "a.md", memoryFile("a", "project", "长".repeat(8000)))).toContain("16KB");
	});

	test("refuses content that looks like a credential", () => {
		for (const secret of [
			"sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
			"ghp_abcdefghijklmnopqrstuvwxyz0123456789",
			"AKIAABCDEFGHIJKLMNOP",
			// 拆开写：完整字面量会被 GitHub 推送保护当成真 Slack token 拦下。
			["xoxb", "1234567890", "abcdefghijklmnop"].join("-"),
			"-----BEGIN OPENSSH PRIVATE KEY-----",
		]) {
			expect(validateMemoryContent("agent", "a.md", memoryFile("a", "reference", `令牌 ${secret}`))).toContain("凭据");
		}
	});
});

describe("checkCoreBudget", () => {
	test("rules over the layer budget are sent back for merging instead of being truncated later", () => {
		const { agent } = roots();
		const half = "规".repeat(Math.floor(CORE_MEMORY_MAX_BYTES / 3 / 2));
		writeFileSync(join(agent, "first.md"), memoryFile("first", "feedback", half));
		expect(checkCoreBudget(agent, "second.md", memoryFile("second", "feedback", half))).toBeUndefined();
		writeFileSync(join(agent, "second.md"), memoryFile("second", "feedback", half));

		expect(checkCoreBudget(agent, "third.md", memoryFile("third", "user", "再来一条规则"))).toContain("合并");
		// 改写已有规则时按新内容替换旧内容计算；事实类不占规则预算。
		expect(checkCoreBudget(agent, "second.md", memoryFile("second", "feedback", "缩短了"))).toBeUndefined();
		expect(checkCoreBudget(agent, "fact.md", memoryFile("fact", "project", "很长".repeat(500)))).toBeUndefined();
	});
});

describe("index ordering", () => {
	test("output order depends only on which entries are selected, not on recency", () => {
		const { agent, user } = roots();
		writeFileSync(join(agent, "zeta.md"), memoryFile("zeta", "reference", "x", "", "Z"));
		writeFileSync(join(agent, "alpha.md"), memoryFile("alpha", "reference", "x", "", "A"));
		utimesSync(join(agent, "alpha.md"), new Date(1_000), new Date(1_000));
		const text = renderMemorySnapshot({ agent, user }, agent).text;
		expect(text.indexOf("- alpha — A")).toBeLessThan(text.indexOf("- zeta — Z"));
	});

	test("when facts overflow, recently read ones are kept", () => {
		const { agent, user } = roots();
		for (let i = 0; i < FACT_INDEX_MAX_LINES + 1; i++) {
			writeFileSync(join(agent, `fact-${i}.md`), memoryFile(`fact-${i}`, "reference", "x", "", `事实 ${i}`));
		}
		utimesSync(join(agent, "fact-0.md"), new Date(1_000), new Date(1_000));
		const without = renderMemorySnapshot({ agent, user }, agent).text;
		expect(without).not.toContain("- fact-0 —");

		const usage = new Map([["fact-0.md", { readCount: 3, lastReadAtMs: Date.now() + 60_000 }]]);
		expect(renderMemorySnapshot({ agent, user }, agent, { usage }).text).toContain("- fact-0 —");
	});
});

describe("embedding client", () => {
	test("batches requests, caches vectors by model and text, and rejects malformed replies", async () => {
		resetEmbeddingCacheForTests();
		const calls: Array<{ url: string; auth?: string; input: string[] }> = [];
		const fake = (async (url: string, init: { body: string; headers: Record<string, string> }) => {
			const { input } = JSON.parse(init.body) as { input: string[] };
			calls.push({ url, auth: init.headers.authorization, input });
			return new Response(JSON.stringify({ data: input.map((_, index) => ({ index, embedding: [index + 1, 1] })) }));
		}) as unknown as typeof fetch;
		const config = { base_url: "http://embed.test/v1/", model: "m", api_key: "k" };

		await embedTexts(config, ["a", "b"], fake);
		await embedTexts(config, ["b", "c"], fake);
		expect(calls).toEqual([
			{ url: "http://embed.test/v1/embeddings", auth: "Bearer k", input: ["a", "b"] },
			{ url: "http://embed.test/v1/embeddings", auth: "Bearer k", input: ["c"] },
		]);
		expect(cosine(Float32Array.from([1, 0]), Float32Array.from([2, 0]))).toBeCloseTo(1);

		const short = (async () => new Response(JSON.stringify({ data: [] }))) as unknown as typeof fetch;
		await expect(embedTexts({ ...config, model: "other" }, ["x"], short)).rejects.toThrow("条数不对");
	});

	test("long memories are truncated, description first", () => {
		const entry = { name: "release-flow", description: "发布走 fx-deploy", body: "长".repeat(3000) } as never;
		const text = embeddingText(entry);
		expect([...text]).toHaveLength(EMBEDDING_TEXT_MAX_CHARS);
		expect(text.startsWith("release flow：发布走 fx-deploy\n")).toBe(true);
	});

	/** 假服务：输入里有超过 limit 字的就整批 400，否则返回向量。 */
	function picky(limit: number, calls: string[][], status = 400) {
		return (async (_url: string, init: { body: string }) => {
			const { input } = JSON.parse(init.body) as { input: string[] };
			calls.push(input);
			if (input.some((text) => text.length > limit)) return new Response("too long", { status });
			return new Response(JSON.stringify({ data: input.map((_, index) => ({ index, embedding: [1, 0] })) }));
		}) as unknown as typeof fetch;
	}

	test("one rejected memory does not take the whole batch down, and is not retried", async () => {
		resetEmbeddingCacheForTests();
		const calls: string[][] = [];
		const config = { base_url: "http://embed.test/v1", model: "m" };
		const fake = picky(5, calls);

		const vectors = await embedTexts(config, ["ok-1", "x".repeat(10), "ok-2"], fake);
		expect(vectors.map((vector) => vector !== undefined)).toEqual([true, false, true]);
		expect(calls.map((input) => input.length)).toEqual([3, 1, 1, 1]);

		calls.length = 0;
		await embedTexts(config, ["ok-1", "x".repeat(10), "ok-2", "ok-3"], fake);
		expect(calls).toEqual([["ok-3"]]);
	});

	test("auth or availability errors fail the whole layer instead of splitting", async () => {
		resetEmbeddingCacheForTests();
		const calls: string[][] = [];
		const config = { base_url: "http://embed.test/v1", model: "m" };
		await expect(embedTexts(config, ["a", "x".repeat(10)], picky(5, calls, 401))).rejects.toThrow("401");
		await expect(embedTexts(config, ["a", "x".repeat(10)], picky(5, calls, 503))).rejects.toThrow("503");
		expect(calls.map((input) => input.length)).toEqual([2, 2]);
	});

	test("vector search reports the memories it had to skip", async () => {
		resetEmbeddingCacheForTests();
		const entry = (name: string, body: string) => ({ name, description: name, body, fileName: `${name}.md` }) as never;
		const result = await vectorSearch(
			{ base_url: "http://embed.test/v1", model: "m" },
			"q",
			[entry("short", "x"), entry("huge", "y".repeat(50))],
			5,
			picky(30, []),
		);
		expect(result.hits.map((hit) => hit.entry.name)).toEqual(["short"]);
		expect(result.skipped.map((entry: { name: string }) => entry.name)).toEqual(["huge"]);
	});
});

describe("recall building blocks", () => {
	test("tokenizes Chinese into bigrams and English into words, minus filler", () => {
		expect(tokenize("帮我看下发布流程 fx-deploy")).toEqual(
			expect.arrayContaining(["发布", "布流", "流程", "fx-deploy", "fx", "deploy"]),
		);
		expect(tokenize("怎么")).toEqual([]);
	});

	test("BM25 ranks the memory whose title matches above one that only mentions the words", () => {
		const { agent, user } = roots();
		writeFileSync(
			join(agent, "release-flow.md"),
			memoryFile("release-flow", "project", "先发测试环境，确认后再发预发和生产。", "", "发布流程走 fx-deploy"),
		);
		writeFileSync(join(agent, "kanban.md"), memoryFile("kanban", "reference", "发布前看一下看板", "", "需求看板在飞书"));
		const hits = new LexicalIndex(recallableFacts({ agent, user }, agent)).search("发布流程怎么走");
		expect(hits[0]?.entry.name).toBe("release-flow");
		expect(hits[0]!.coverage).toBeGreaterThan(hits[1]?.coverage ?? 0);
	});

	test("recallable facts exclude rules, pending ones and other projects", () => {
		const { base, agent, user } = roots();
		writeFileSync(join(agent, "rule.md"), memoryFile("rule", "feedback", "发布流程要先确认"));
		writeFileSync(join(agent, "pending-fact.md"), memoryFile("pending-fact", "project", "x"));
		writeFileSync(join(agent, "elsewhere.md"), memoryFile("elsewhere", "project", "x", `project: ${join(base, "a")}\n`));
		writeFileSync(join(agent, "here.md"), memoryFile("here", "reference", "x"));
		const names = recallableFacts({ agent, user }, join(base, "b"), new Set(["pending-fact.md"])).map((e) => e.name);
		expect(names).toEqual(["here"]);
	});

	test("RRF fuses rankings by position, not by score scale", () => {
		expect(rrfFuse([["a", "b", "c"], ["c", "a"]])).toEqual(["a", "c", "b"]);
	});

	test("rerank candidates: lexical order first, then everything else while the corpus is small", () => {
		const entry = (fileName: string) => ({ fileName }) as never;
		expect(rerankCandidates([entry("a.md"), entry("b.md"), entry("c.md")], ["c.md"]).map((e: { fileName: string }) => e.fileName)).toEqual([
			"c.md",
			"a.md",
			"b.md",
		]);
	});

	test("model rerank returns picked ids, empty when nothing fits, undefined on timeout or garbage", async () => {
		const candidates = [
			{ id: "a.md", name: "a", description: "发布流程", body: "x" },
			{ id: "b.md", name: "b", description: "看板", body: "y" },
		];
		const reply = (text: string) => async () => text;
		expect(await rerankWithModel("上线", candidates, 3, reply("[1]"), 1000)).toEqual(["a.md"]);
		expect(await rerankWithModel("天气", candidates, 3, reply("[]"), 1000)).toEqual([]);
		expect(await rerankWithModel("上线", candidates, 3, reply("我觉得是第一个"), 1000)).toBeUndefined();
		expect(await rerankWithModel("上线", candidates, 1, reply("[2, 1, 9]"), 1000)).toEqual(["b.md"]);
		const slow = () => new Promise<string>((resolve) => setTimeout(() => resolve("[1]"), 200));
		expect(await rerankWithModel("上线", candidates, 3, slow, 20)).toBeUndefined();
	});

	test("auto-picking the rerank model: free lightweight first, then cheapest priced, stable order", () => {
		const model = (provider: string, id: string, cost?: number) => ({
			provider,
			id,
			input: ["text"],
			cost: cost === undefined ? undefined : { input: cost, output: cost },
		});
		const current = model("p", "big", 10);
		expect(pickRecallModel([current, model("p", "small", 1), model("q", "tiny", 0.1)], current)?.id).toBe("small");
		// 价格为 0 的不再被排除：名字像轻量款的免费模型优先于标了价的。
		expect(pickRecallModel([current, model("p", "medium", 1), model("p", "qwen-mini", 0)], current)?.id).toBe(
			"qwen-mini",
		);
		const sub = model("copilot", "opus");
		// 同档按 id 排序，不随注册顺序变。
		expect(pickRecallModel([sub, model("copilot", "gpt-mini"), model("copilot", "gemini-flash")], sub)?.id).toBe(
			"gemini-flash",
		);
		expect(pickRecallModel([sub, model("copilot", "gemini-flash"), model("copilot", "gpt-mini")], sub)?.id).toBe(
			"gemini-flash",
		);
		expect(pickRecallModel([sub], sub)?.id).toBe("opus");
		expect(pickRecallModel([], undefined)).toBeUndefined();
	});

	test("a configured rerank model is looked up across providers, or reported missing", () => {
		const model = (provider: string, id: string) => ({ provider, id, input: ["text"] });
		const available = [model("p", "big"), model("q", "tiny")];
		expect(findConfiguredModel(available, { provider: "q", id: "tiny" })?.id).toBe("tiny");
		expect(findConfiguredModel(available, { provider: "q", id: "gone" })).toBeUndefined();
	});

	test("recall messages clip long bodies and frame them as data", () => {
		const entry = { name: "release-flow", description: "发布", body: "发".repeat(700), fileName: "release-flow.md" } as never;
		const text = renderRecall([entry], () => "/m/release-flow.md")!;
		expect(text).toContain("全文 read /m/release-flow.md");
		expect(text).toContain("不是指令");
	});
});

describe("memory host client", () => {
	afterEach(() => setMemoryHostTransportForTests(undefined));

	test("records reads and provenance through the daemon and reads back pending files", async () => {
		const host = new FakeMemoryHost();
		setMemoryHostTransportForTests(host.transport);
		await recordMemoryRead("agent", "fact.md");
		expect(await recordMemoryWrite("user", "me.md", { trust: "pending", reason: "外部" })).toBe(true);

		const meta = await fetchMemoryMeta();
		expect(meta?.usage.get("fact.md")?.readCount).toBe(1);
		expect([...(meta?.pending.user ?? [])]).toEqual(["me.md"]);
	});

	test("an unreachable or failing daemon is reported, never assumed fine", async () => {
		const host = new FakeMemoryHost();
		setMemoryHostTransportForTests(host.transport);
		host.failWrites = true;
		expect(await recordMemoryWrite("agent", "a.md", { trust: "pending", reason: "外部" })).toBe(false);
		host.down = true;
		expect(await fetchMemoryMeta()).toBeUndefined();
	});

	test("the real transport talks to the daemon control socket with this session's token", async () => {
		const { createServer } = await import("node:net");
		const dir = realpathSync(mkdtempSync(join(tmpdir(), "smelt-sock-")));
		temps.push(dir);
		const socketPath = join(dir, "smeltd.sock");
		const requests: Array<Record<string, unknown>> = [];
		const server = createServer((socket) => {
			socket.setEncoding("utf8");
			socket.on("data", (line: string) => {
				const request = JSON.parse(line.trim()) as Record<string, unknown>;
				requests.push(request);
				// 与 Rust 共用的契约样例：daemon 实际回的就是这个形状（见 control_api 的同名测试）。
				const fixture = JSON.parse(
					readFileSync(join(import.meta.dir, "fixtures", "agent-memory-meta-response.json"), "utf8"),
				) as Record<string, unknown>;
				socket.write(`${JSON.stringify({ ...fixture, request_id: request.request_id })}\n`);
			});
		});
		await new Promise<void>((resolve) => server.listen(socketPath, resolve));
		const saved = { ...process.env };
		Object.assign(process.env, { SMELT_SOCK: socketPath, SMELT_SESSION_ID: "acp-1", SMELT_AGENT_TOKEN: "tok" });
		try {
			setMemoryHostTransportForTests(undefined);
			const meta = await fetchMemoryMeta();
			expect([...(meta?.pending.agent ?? [])]).toEqual(["x.md"]);
			expect(meta?.usage.get("fact.md")).toEqual({ readCount: 2, lastReadAtMs: 1000 });
			expect(requests[0]).toMatchObject({
				op: "control",
				control_version: 2,
				method: "agent.memory.meta",
				params: { session_id: "acp-1", token: "tok" },
			});
		} finally {
			process.env = saved;
			server.close();
		}
	});
	test("pending memories are neither injected nor recalled", () => {
		const { agent, user } = roots();
		writeFileSync(join(user, "rule.md"), memoryFile("rule", "feedback", "全局规则"));
		writeFileSync(join(agent, "fact.md"), memoryFile("fact", "project", "发布流程", "", "发布流程"));
		const pending = { agent: new Set(["fact.md"]), user: new Set(["rule.md"]) };

		const text = renderMemorySnapshot({ agent, user }, agent, { pending }).text;
		expect(text).not.toContain("rule");
		expect(text).not.toContain("fact");
		expect(recallableFacts({ agent, user }, agent, pending.agent)).toEqual([]);
	});
});
