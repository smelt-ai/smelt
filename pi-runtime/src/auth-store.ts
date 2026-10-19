/**
 * `~/.pi/agent/auth.json` as a pi-ai `CredentialStore`.
 *
 * Pi's own `AuthStorage` is not exported from `@earendil-works/pi-coding-agent`
 * (its package exports are `.`, `./rpc-entry`, `./client`), so the helper has
 * to write the file itself. Everything here mirrors what Pi does, because both
 * processes write the same file: `proper-lockfile` with `realpath: false` for
 * the cross-process lock, `0600` on the file, `0700` on the directory, and
 * two-space JSON so a login from Smelt leaves a diff Pi would have produced.
 *
 * Values are stored raw. Pi resolves `$ENV` / `!command` key forms when it
 * reads; a login writes a literal credential, so there is nothing to resolve
 * on this path.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { Credential, CredentialInfo, CredentialStore } from "@earendil-works/pi-ai";
import lockfile from "proper-lockfile";

const FILE_OPTIONS = { encoding: "utf-8", mode: 0o600 } as const;
const LOCK_STALE_MS = 30_000;

export class AuthJsonCredentialStore implements CredentialStore {
	constructor(private readonly authPath: string) {}

	async read(providerId: string): Promise<Credential | undefined> {
		return this.readAll()[providerId];
	}

	async list(): Promise<readonly CredentialInfo[]> {
		return Object.entries(this.readAll()).map(([providerId, credential]) => ({
			providerId,
			type: credential?.type === "oauth" ? "oauth" : "api_key",
		}));
	}

	async modify(
		providerId: string,
		fn: (current: Credential | undefined) => Promise<Credential | undefined>,
	): Promise<Credential | undefined> {
		return this.withLock(async () => {
			const all = this.readAll();
			const next = await fn(all[providerId]);
			if (next === undefined) return all[providerId];
			all[providerId] = next;
			this.writeAll(all);
			return next;
		});
	}

	async delete(providerId: string): Promise<void> {
		await this.withLock(async () => {
			const all = this.readAll();
			if (!(providerId in all)) return undefined;
			delete all[providerId];
			this.writeAll(all);
			return undefined;
		});
	}

	private readAll(): Record<string, Credential> {
		if (!existsSync(this.authPath)) return {};
		let raw: string;
		try {
			raw = readFileSync(this.authPath, "utf-8").replace(/^\uFEFF/, "");
		} catch {
			return {};
		}
		if (!raw.trim()) return {};
		let parsed: unknown;
		try {
			parsed = JSON.parse(raw);
		} catch {
			// A corrupt file must not be silently replaced: refuse the write path
			// instead of dropping every other provider's credential.
			throw new Error(`${this.authPath} is not valid JSON; fix or remove it first`);
		}
		if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return {};
		return parsed as Record<string, Credential>;
	}

	private writeAll(all: Record<string, Credential>): void {
		// No trailing newline: Pi writes exactly this, and a login from Smelt
		// should not show up as a whole-file diff to it.
		writeFileSync(this.authPath, JSON.stringify(all, null, 2), FILE_OPTIONS);
	}

	private ensureFile(): void {
		const dir = dirname(this.authPath);
		if (!existsSync(dir)) mkdirSync(dir, { recursive: true, mode: 0o700 });
		if (!existsSync(this.authPath)) writeFileSync(this.authPath, "{}", FILE_OPTIONS);
	}

	private async withLock<T>(fn: () => Promise<T>): Promise<T> {
		this.ensureFile();
		const release = await lockfile.lock(this.authPath, {
			realpath: false,
			stale: LOCK_STALE_MS,
			retries: { retries: 10, minTimeout: 20, maxTimeout: 500 },
		});
		try {
			return await fn();
		} finally {
			await release();
		}
	}
}
