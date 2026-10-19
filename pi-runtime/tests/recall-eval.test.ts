import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { scanMemoryLayer } from "../src/memory-files.ts";
import { EVERY_TURN_MIN_COVERAGE, LexicalIndex } from "../src/memory-recall.ts";

/**
 * 中文召回评测：合成的记忆 + 直说 / 换说法两类提问 + 不该召回的提问。
 * 衡量 Recall@3（期望的记忆是否在前 3 条）。语义层（向量、模型重排）离线测不了，这里只评
 * 关键词层；模型重排用 scripts/recall-eval-live.ts 拿真实模型评。
 *
 * 2026-10 基线：旧的字面重合打分 Recall@3 73%（直说 100%，换说法 46%）；BM25 77%（换说法 54%）；
 * 便宜模型重排（github-copilot/gpt-6-luna）直说、换说法、无关问题全对，p50 1.5s / p90 2.6s。
 */
type Eval = {
	memories: Array<{ name: string; type: string; description: string; body: string }>;
	queries: Array<{ query: string; expect: string[]; kind: "literal" | "paraphrase" }>;
	negatives: string[];
};
const evalSet = JSON.parse(readFileSync(join(import.meta.dir, "fixtures", "recall-eval.json"), "utf8")) as Eval;

const base = realpathSync(mkdtempSync(join(tmpdir(), "smelt-recall-eval-")));
const agent = join(base, "agent");
const user = join(base, "user");
mkdirSync(agent);
mkdirSync(user);
for (const memory of evalSet.memories) {
	writeFileSync(
		join(agent, `${memory.name}.md`),
		`---\nname: ${memory.name}\ndescription: ${memory.description}\ntype: ${memory.type}\n---\n${memory.body}\n`,
	);
}
afterAll(() => rmSync(base, { recursive: true, force: true }));

function recallAt3(rank: (query: string) => string[], kind?: "literal" | "paraphrase"): number {
	const queries = evalSet.queries.filter((query) => !kind || query.kind === kind);
	const hits = queries.filter((query) => rank(query.query).slice(0, 3).some((name) => query.expect.includes(name)));
	return hits.length / queries.length;
}

const index = new LexicalIndex(scanMemoryLayer(agent));
const bm25 = (query: string) => index.search(query, 3).map((hit) => hit.entry.name);
describe("recall eval (Chinese)", () => {
	test("BM25 finds every literal question and holds the recorded baseline", () => {
		const report = { all: recallAt3(bm25), literal: recallAt3(bm25, "literal"), paraphrase: recallAt3(bm25, "paraphrase") };
		console.log("bm25 recall@3", JSON.stringify(report));
		expect(report.literal).toBe(1);
		expect(report.all).toBeGreaterThanOrEqual(0.76);
	});

	test("the every-turn threshold injects nothing wrong and nothing for unrelated questions", () => {
		const confident = evalSet.queries
			.map((query) => ({ query, top: index.search(query.query, 1)[0] }))
			.filter(({ top }) => top && top.coverage >= EVERY_TURN_MIN_COVERAGE);
		const wrong = confident.filter(({ query, top }) => !query.expect.includes(top!.entry.name));
		console.log("every-turn precision", `${confident.length - wrong.length}/${confident.length}`);
		expect(wrong).toEqual([]);
		expect(confident.length).toBeGreaterThanOrEqual(10);
		for (const negative of evalSet.negatives) {
			expect((index.search(negative, 1)[0]?.coverage ?? 0) < EVERY_TURN_MIN_COVERAGE).toBe(true);
		}
	});
});
