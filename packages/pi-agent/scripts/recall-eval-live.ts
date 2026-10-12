/**
 * 用真实模型评测召回的模型重排层（不进 CI，需要本机 Pi 凭据）：
 *   bun scripts/recall-eval-live.ts [provider/model]
 * 默认用 ~/.pi/agent/settings.json 的默认模型所在提供方里，pickRecallModel 挑出的便宜模型。
 */
import { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { scanMemoryLayer } from "../src/memory-files.ts";
import { LexicalIndex, pickRecallModel, type RecallCandidate, rerankWithModel } from "../src/memory-recall.ts";

const evalSet = JSON.parse(readFileSync(join(import.meta.dir, "..", "tests", "fixtures", "recall-eval.json"), "utf8"));
const dir = mkdtempSync(join(tmpdir(), "recall-live-"));
for (const m of evalSet.memories) {
	writeFileSync(join(dir, `${m.name}.md`), `---\nname: ${m.name}\ndescription: ${m.description}\ntype: ${m.type}\n---\n${m.body}\n`);
}
const entries = scanMemoryLayer(dir);
const index = new LexicalIndex(entries);
const agentDir = join(homedir(), ".pi", "agent");
const runtime = await ModelRuntime.create({ authPath: join(agentDir, "auth.json"), modelsPath: join(agentDir, "models.json") });
const settings = JSON.parse(readFileSync(join(agentDir, "settings.json"), "utf8"));
const all = await runtime.getAvailable();
const [forcedProvider, forcedId] = (process.argv[2] ?? "").split("/");
const current = all.find((m) => m.provider === (forcedProvider || settings.defaultProvider) && m.id === (forcedId || settings.defaultModel));
const model = forcedId ? current : pickRecallModel(all, current);
if (!model) throw new Error("找不到模型");
console.log("rerank model:", `${model.provider}/${model.id}`);

const candidates: RecallCandidate[] = entries.map((e) => ({ id: e.name, name: e.name, description: e.description, body: e.body }));
const ask = async (system: string, user: string, signal: AbortSignal) => {
	const reply = await runtime.completeSimple(model, { systemPrompt: system, messages: [{ role: "user", content: user, timestamp: Date.now() }] }, {
		signal, maxTokens: 200,
	});
	if (reply.stopReason === "error") throw new Error(reply.errorMessage);
	return reply.content.filter((c) => c.type === "text").map((c) => (c as { text: string }).text).join("");
};

const result: Record<string, { hit: number; total: number }> = {};
const times: number[] = [];
let failures = 0;
for (const q of [...evalSet.queries, ...evalSet.negatives.map((n: string) => ({ query: n, expect: [], kind: "negative" }))]) {
	const started = performance.now();
	const picked = await rerankWithModel(q.query, candidates, 3, ask, 8000);
	times.push(performance.now() - started);
	if (picked === undefined) failures++;
	const lexical = index.search(q.query, 3).map((h) => h.entry.name);
	const ranking = picked ?? lexical;
	const ok = q.kind === "negative" ? ranking.length === 0 : ranking.some((n) => q.expect.includes(n));
	const bucket = (result[q.kind] ??= { hit: 0, total: 0 });
	bucket.total++;
	if (ok) bucket.hit++;
	if (!ok) console.log("  ✗", q.kind, q.query, "→", JSON.stringify(ranking));
}
times.sort((a, b) => a - b);
console.log(JSON.stringify({ result, failures, p50ms: Math.round(times[Math.floor(times.length / 2)]!), p90ms: Math.round(times[Math.floor(times.length * 0.9)]!) }));
rmSync(dir, { recursive: true, force: true });
