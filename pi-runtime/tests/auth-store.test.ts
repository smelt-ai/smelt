import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AuthJsonCredentialStore } from "../src/auth-store.ts";

function store(): { path: string; store: AuthJsonCredentialStore } {
	const path = join(mkdtempSync(join(tmpdir(), "smelt-auth-")), "auth.json");
	return { path, store: new AuthJsonCredentialStore(path) };
}

describe("auth.json 凭据库", () => {
	test("写下的形状和权限与 Pi 自己写的一致", async () => {
		const { path, store: subject } = store();
		await subject.modify("github-copilot", async () => ({
			type: "oauth",
			refresh: "r",
			access: "a",
			expires: 42,
		}));
		// Pi 写的是两空格缩进、无尾换行的 JSON 对象，文件权限 0600。
		expect(readFileSync(path, "utf-8")).toBe(
			'{\n  "github-copilot": {\n    "type": "oauth",\n    "refresh": "r",\n    "access": "a",\n    "expires": 42\n  }\n}',
		);
		expect(statSync(path).mode & 0o777).toBe(0o600);
	});

	test("只动自己那个 provider，别人的凭据原样留着", async () => {
		const { path, store: subject } = store();
		writeFileSync(path, JSON.stringify({ anthropic: { type: "api_key", key: "sk" } }, null, 2));
		await subject.modify("xai", async () => ({ type: "oauth", refresh: "r", access: "a", expires: 1 }));
		await subject.delete("xai");
		expect(JSON.parse(readFileSync(path, "utf-8"))).toEqual({
			anthropic: { type: "api_key", key: "sk" },
		});
	});

	test("回调返回 undefined 表示不改动", async () => {
		const { path, store: subject } = store();
		writeFileSync(path, JSON.stringify({ xai: { type: "api_key", key: "sk" } }, null, 2));
		const result = await subject.modify("xai", async () => undefined);
		expect(result).toEqual({ type: "api_key", key: "sk" });
		expect(JSON.parse(readFileSync(path, "utf-8"))).toEqual({ xai: { type: "api_key", key: "sk" } });
	});

	test("列出来的类型区分 oauth 与 api key", async () => {
		const { path, store: subject } = store();
		writeFileSync(
			path,
			JSON.stringify({
				anthropic: { type: "oauth", refresh: "r", access: "a", expires: 1 },
				openai: { type: "api_key", key: "sk" },
			}),
		);
		expect(await subject.list()).toEqual([
			{ providerId: "anthropic", type: "oauth" },
			{ providerId: "openai", type: "api_key" },
		]);
	});

	test("文件坏了就报错，绝不覆盖", async () => {
		const { path, store: subject } = store();
		writeFileSync(path, "{ not json");
		// 一份存着多个账号的 auth.json 被静默重写，等于把用户其它登录全弄丢。
		expect(subject.modify("xai", async () => ({ type: "api_key", key: "sk" }))).rejects.toThrow(
			/not valid JSON/,
		);
		expect(readFileSync(path, "utf-8")).toBe("{ not json");
	});
});
