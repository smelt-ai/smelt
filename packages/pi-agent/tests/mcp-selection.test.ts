import { describe, expect, test } from "bun:test";
import {
	SMELT_AGENT_MCP_SERVERS_ENV,
	SMELT_MCP_EXTENSION_NAME,
	consumeMcpSelection,
	selectMcpServers,
} from "../src/mcp-selection.ts";
import { SMELT_EXTENSION_FACTORIES, withAgentMcp } from "../src/runtime-extensions.ts";

describe("agent MCP selection", () => {
	test("is consumed so tool subprocesses do not inherit it", () => {
		const environment: Record<string, string | undefined> = {
			[SMELT_AGENT_MCP_SERVERS_ENV]: JSON.stringify(["figma", "", 3, "rea"]),
		};
		expect([...(consumeMcpSelection(environment) ?? [])]).toEqual(["figma", "rea"]);
		expect(environment[SMELT_AGENT_MCP_SERVERS_ENV]).toBeUndefined();
	});

	test("is absent outside product agents and on malformed input", () => {
		expect(consumeMcpSelection({})).toBeUndefined();
		expect(consumeMcpSelection({ [SMELT_AGENT_MCP_SERVERS_ENV]: "not json" })).toBeUndefined();
		expect(consumeMcpSelection({ [SMELT_AGENT_MCP_SERVERS_ENV]: "{}" })).toBeUndefined();
	});

	test("keeps only checked, enabled entries of mcp.json", () => {
		const document = {
			autoEnableCodemode: false,
			mcpServers: {
				figma: { url: "https://mcp.figma.com/mcp" },
				rea: { command: "bun", args: ["x", "rea-agents@6.5.0", "mcp"] },
				off: { command: "npx", enabled: false },
				broken: "not an object",
			},
		};
		const selected = selectMcpServers(document, new Set(["rea", "off", "broken", "missing"]));
		expect(selected.map(([name]) => name)).toEqual(["rea"]);
		expect(selected[0][1]).toEqual(document.mcpServers.rea);
		expect(selectMcpServers(undefined, new Set(["rea"]))).toEqual([]);
		expect(selectMcpServers({ mcpServers: [] }, new Set(["0"]))).toEqual([]);
	});

	test("adds the extension right before permission only when something is checked", () => {
		expect(withAgentMcp(SMELT_EXTENSION_FACTORIES, undefined)).toEqual([...SMELT_EXTENSION_FACTORIES]);
		expect(withAgentMcp(SMELT_EXTENSION_FACTORIES, new Set())).toEqual([...SMELT_EXTENSION_FACTORIES]);

		const names = withAgentMcp(SMELT_EXTENSION_FACTORIES, new Set(["figma"])).map(
			(input) => (typeof input === "function" ? "" : input.name),
		);
		expect(names.at(-1)).toBe("smelt-permission");
		expect(names.at(-2)).toBe(SMELT_MCP_EXTENSION_NAME);
		expect(names.length).toBe(SMELT_EXTENSION_FACTORIES.length + 1);
	});
});
