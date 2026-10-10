import { Database } from "bun:sqlite";
import { join } from "node:path";

/**
 * 记忆使用统计：模型 read 过哪条记忆、多少次、最近一次什么时候。
 *
 * 业务数据按项目约定进 SQLite。每个记忆目录一份 `.usage.sqlite`：跟着记忆走，删智能体时
 * 随目录一起删；文件名以点开头，不会被当成记忆。多个 Pi 进程可能同时写，靠 busy_timeout
 * 排队。统计失败绝不影响工具调用本身。
 */
export const MEMORY_USAGE_DB = ".usage.sqlite";

export type MemoryUsage = { readCount: number; lastReadAtMs: number };

function open(root: string, readonly: boolean): Database {
	const db = new Database(join(root, MEMORY_USAGE_DB), readonly ? { readonly: true } : { create: true });
	db.exec("PRAGMA busy_timeout = 2000");
	if (!readonly) {
		db.exec(`CREATE TABLE IF NOT EXISTS memory_usage (
			file_name TEXT PRIMARY KEY,
			read_count INTEGER NOT NULL,
			last_read_at_ms INTEGER NOT NULL
		) STRICT`);
	}
	return db;
}

export function recordMemoryRead(root: string, fileName: string, nowMs = Date.now()): void {
	let db: Database | undefined;
	try {
		db = open(root, false);
		db.query(
			`INSERT INTO memory_usage (file_name, read_count, last_read_at_ms) VALUES (?1, 1, ?2)
			 ON CONFLICT(file_name) DO UPDATE SET read_count = read_count + 1, last_read_at_ms = ?2`,
		).run(fileName, nowMs);
	} catch {
		// 统计是锦上添花：数据库锁住、磁盘只读都不该打断模型读记忆。
	} finally {
		db?.close();
	}
}

export function readMemoryUsage(root: string): Map<string, MemoryUsage> {
	const usage = new Map<string, MemoryUsage>();
	let db: Database | undefined;
	try {
		db = open(root, true);
		const rows = db
			.query("SELECT file_name, read_count, last_read_at_ms FROM memory_usage")
			.all() as Array<{ file_name: string; read_count: number; last_read_at_ms: number }>;
		for (const row of rows) {
			usage.set(row.file_name, { readCount: row.read_count, lastReadAtMs: row.last_read_at_ms });
		}
	} catch {
		// 还没有统计库，或读不了：当作没有使用记录。
	} finally {
		db?.close();
	}
	return usage;
}
