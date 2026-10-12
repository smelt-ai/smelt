import type { MemoryHostTransport } from "../src/memory-host.ts";

type Provenance = { trust: "trusted" | "pending"; reason: string };

/** 测试用的宿主：按 Rust `agent_memory::handle_host_request` 的协议在内存里记元数据。 */
export class FakeMemoryHost {
	readonly usage = { agent: new Map<string, number>(), user: new Map<string, number>() };
	readonly provenance = { agent: new Map<string, Provenance>(), user: new Map<string, Provenance>() };
	/** 模拟宿主不可达：所有请求都拿不到结果。 */
	down = false;
	/** 模拟写来源失败（库锁住、磁盘满）。 */
	failWrites = false;

	/** 按 daemon `agent.memory.*` 控制方法的契约应答；失败返回 undefined。 */
	readonly transport: MemoryHostTransport = async (method, params) => {
		if (this.down) return undefined;
		const layer = params.layer as "agent" | "user";
		const file = params.file as string;
		if (method === "agent.memory.meta") {
			const pending = (which: "agent" | "user") =>
				[...this.provenance[which]].filter(([, value]) => value.trust === "pending").map(([name]) => name);
			return {
				agent: {
					usage: [...this.usage.agent].map(([name, count]) => ({ file: name, read_count: count, last_read_at_ms: 0 })),
					pending: pending("agent"),
				},
				user: { usage: [], pending: pending("user") },
			};
		}
		if (method === "agent.memory.record_read") {
			this.usage[layer].set(file, (this.usage[layer].get(file) ?? 0) + 1);
			return {};
		}
		if (method === "agent.memory.record_write") {
			if (this.failWrites) return undefined;
			this.provenance[layer].set(file, {
				trust: params.trust as "trusted" | "pending",
				reason: params.reason as string,
			});
			return {};
		}
		return undefined;
	};

	approve(layer: "agent" | "user", file: string): void {
		this.provenance[layer].set(file, { trust: "trusted", reason: "用户采纳" });
	}
}
