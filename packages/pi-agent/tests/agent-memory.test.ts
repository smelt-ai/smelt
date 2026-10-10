import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
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
	recallRelevantFacts,
	recallTokens,
	renderMemorySnapshot,
	resolveMemoryTarget,
	validateMemoryContent,
} from "../src/memory-files.ts";
import { MEMORY_USAGE_DB, readMemoryUsage, recordMemoryRead } from "../src/memory-usage.ts";

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
		expect(renderMemorySnapshot({ agent, user }, agent, usage).text).toContain("- fact-0 —");
	});
});

describe("memory usage", () => {
	test("reads are counted per file in a hidden SQLite file next to the memories", () => {
		const { agent } = roots();
		expect(readMemoryUsage(agent).size).toBe(0);
		recordMemoryRead(agent, "release-flow.md", 1_000);
		recordMemoryRead(agent, "release-flow.md", 2_000);
		recordMemoryRead(agent, "weekly.md", 1_500);

		const usage = readMemoryUsage(agent);
		expect(usage.get("release-flow.md")).toEqual({ readCount: 2, lastReadAtMs: 2_000 });
		expect(usage.get("weekly.md")).toEqual({ readCount: 1, lastReadAtMs: 1_500 });
		expect(MEMORY_USAGE_DB.startsWith(".")).toBe(true);
	});

	test("a broken usage database never throws", () => {
		const { agent } = roots();
		writeFileSync(join(agent, MEMORY_USAGE_DB), "not a database");
		expect(() => recordMemoryRead(agent, "a.md")).not.toThrow();
		expect(readMemoryUsage(agent).size).toBe(0);
	});
});

describe("recallRelevantFacts", () => {
	test("tokenizes Chinese into bigrams and English into words, minus filler", () => {
		expect([...recallTokens("帮我看下发布流程 deploy")].sort()).toEqual(["下发", "发布", "布流", "我看", "deploy", "流程", "看下"].sort());
	});

	test("attaches the bodies of the most relevant facts only", () => {
		const { agent, user } = roots();
		writeFileSync(
			join(agent, "release-flow.md"),
			memoryFile("release-flow", "project", "先发测试环境，确认后再发预发和生产。", "", "发布走 fx-deploy 流水线"),
		);
		writeFileSync(join(agent, "kanban.md"), memoryFile("kanban", "reference", "看板链接在飞书", "", "需求看板在飞书"));
		writeFileSync(join(agent, "rule.md"), memoryFile("rule", "feedback", "发布流程要先确认"));

		const recalled = recallRelevantFacts({ agent, user }, agent, "这次的发布流程怎么走？");

		expect(recalled).toContain("### release-flow — 发布走 fx-deploy 流水线");
		expect(recalled).toContain("先发测试环境");
		expect(recalled).not.toContain("kanban");
		// 规则类本来就常驻，不重复召回。
		expect(recalled).not.toContain("### rule");
		expect(recallRelevantFacts({ agent, user }, agent, "今天天气不错")).toBeUndefined();
	});

	test("long bodies are clipped with a pointer to the full file", () => {
		const { agent, user } = roots();
		writeFileSync(
			join(agent, "release-flow.md"),
			memoryFile("release-flow", "project", "发布".repeat(500), "", "发布流程细节"),
		);
		const recalled = recallRelevantFacts({ agent, user }, agent, "发布流程")!;
		expect(recalled).toContain(`全文 read ${join(agent, "release-flow.md")}`);
		expect(Buffer.byteLength(recalled)).toBeLessThan(2_200);
	});

	test("facts of another project are not recalled", () => {
		const { base, agent, user } = roots();
		writeFileSync(
			join(agent, "a-flow.md"),
			memoryFile("a-flow", "project", "A 项目发布流程", `project: ${join(base, "a")}\n`, "A 项目发布流程"),
		);
		expect(recallRelevantFacts({ agent, user }, join(base, "b"), "发布流程")).toBeUndefined();
		expect(recallRelevantFacts({ agent, user }, join(base, "a"), "发布流程")).toContain("a-flow");
	});
});
