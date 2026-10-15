import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { StringDecoder } from "node:string_decoder";
import { Type, type Message } from "@earendil-works/pi-ai";
import type {
	ExtensionAPI,
	ExtensionContext,
	RpcExtensionUIRequest,
	RpcExtensionUIResponse,
} from "@earendil-works/pi-coding-agent";
import {
  SMELT_BACKGROUND_TASK_OWNER_ENV,
  SMELT_BACKGROUND_TASK_STOP_TOOL_NAME,
  attachBackgroundTask,
  backgroundTaskOwner,
  endBackgroundTaskOwner,
} from "../background-task.ts";
import { getConsumedPluginArgs } from "../plugin-args.ts";
import { type AgentConfig, type AgentScope, discoverAgents, formatAgentCatalog } from "./agents.ts";
import { subagentResultMessage } from "./result.ts";
import { bridgeSubagentUiRequest } from "./ui-bridge.ts";

const MAX_PARALLEL_TASKS = 8;
const MAX_CONCURRENCY = 4;
const OUTPUT_CAP = 32_000;
const backgroundRuns = new Set<AbortController>();
let subagentSeq = 0;

type Usage = {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cost: number;
  contextTokens: number;
  turns: number;
};

type AgentSource = "user" | "project" | "inline" | "unknown";

type AgentSpec = {
  agent?: string;
  task: string;
  instructions?: string;
  tools?: string[];
  model?: string;
  label?: string;
  cwd?: string;
};

type ResolvedAgent = {
  name: string;
  source: AgentSource;
  systemPrompt: string;
  tools?: string[];
  model?: string;
};

const DEFAULT_INSTRUCTIONS = [
  "You are an isolated subagent. Complete the assigned task autonomously.",
  "Return a concise result the parent agent can use without re-doing your work.",
  "Do not ask the parent to continue your tool calls.",
].join("\n");

type AgentResult = {
  agent: string;
  agentSource: AgentSource;
  task: string;
  exitCode: number;
  stderr: string;
  usage: Usage;
  /** Pi assistant messages consumed by Smelt's nested subagent transcript renderer. */
  messages: Message[];
  output: string;
  model?: string;
  stopReason?: string;
  errorMessage?: string;
  step?: number;
};

type SubagentDetails = {
  mode: "single" | "parallel" | "chain";
  results: AgentResult[];
};

function emptyUsage(): Usage {
  return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0, contextTokens: 0, turns: 0 };
}

/** 单任务和并行默认在后台跑。chain 要吃上一步输出，必须挡住这一轮。 */
export function subagentShouldBackground(input: { background?: boolean; chain?: readonly unknown[] }): boolean {
	if ((input.chain?.length ?? 0) > 0) return false;
	return input.background !== false;
}

function failed(result: AgentResult): boolean {
  return result.exitCode !== 0 || result.stopReason === "error" || result.stopReason === "aborted";
}

function cap(text: string): string {
  if (text.length <= OUTPUT_CAP) return text;
  return `${text.slice(0, OUTPUT_CAP)}\n\n[truncated ${text.length - OUTPUT_CAP} chars]`;
}

/**
 * 子智能体在会话宿主里的归属：父归属下的一个新分支。子进程照样用宿主跑命令（长命令也能
 * 转后台），但它的任务和完成通知都归它自己，不进父会话。
 */
export function childTaskOwner(parentOwner: string, runId: string): string {
  return `${parentOwner}/${runId}`;
}

/** 后台任务列表里那一行写的「在做什么」：并行时列出各任务的名字，单个时就是任务本身。 */
export function backgroundSubagentCommand(params: {
  task?: string;
  tasks?: ReadonlyArray<{ task: string; label?: string; agent?: string }>;
}): string {
  if (params.tasks?.length) {
    return params.tasks.map((task) => task.label || task.agent || task.task).join(" · ");
  }
  return params.task ?? "子智能体";
}

export function subagentEnvironment(environment: NodeJS.ProcessEnv, owner: string): NodeJS.ProcessEnv {
  return { ...environment, [SMELT_BACKGROUND_TASK_OWNER_ENV]: owner };
}

function getPiInvocation(args: string[]): { command: string; args: string[] } {
  const currentScript = process.argv[1];
  const isBunVirtualScript = currentScript?.startsWith("/$bunfs/root/");
  if (currentScript && !isBunVirtualScript && fs.existsSync(currentScript)) {
    return { command: process.execPath, args: [path.resolve(currentScript), ...args] };
  }
  const execName = path.basename(process.execPath).toLowerCase();
  if (!/^(node|bun)(\.exe)?$/.test(execName)) {
    return { command: process.execPath, args };
  }
  return { command: "pi", args };
}

async function mapWithConcurrency<T, R>(
  items: T[],
  concurrency: number,
  fn: (item: T, index: number) => Promise<R>,
): Promise<R[]> {
  if (items.length === 0) return [];
  const limit = Math.max(1, Math.min(concurrency, items.length));
  const results = new Array<R>(items.length);
  let next = 0;
  await Promise.all(
    Array.from({ length: limit }, async () => {
      while (true) {
        const index = next++;
        if (index >= items.length) return;
        results[index] = await fn(items[index], index);
      }
    }),
  );
  return results;
}

function resolveAgent(spec: AgentSpec, agents: AgentConfig[]): { ok: true; agent: ResolvedAgent } | { ok: false; error: string } {
  if (spec.agent) {
    const preset = agents.find((item) => item.name === spec.agent);
    if (!preset) {
      return {
        ok: false,
        error: `Unknown agent "${spec.agent}". Available: ${formatAgentCatalog(agents)}.`,
      };
    }
    return {
      ok: true,
      agent: {
        name: spec.label || preset.name,
        source: preset.source,
        systemPrompt: spec.instructions?.trim()
          ? `${preset.systemPrompt}\n\nAdditional instructions:\n${spec.instructions.trim()}`
          : preset.systemPrompt,
        tools: spec.tools ?? preset.tools,
        model: spec.model ?? preset.model,
      },
    };
  }
  return {
    ok: true,
    agent: {
      name: spec.label || "ad-hoc",
      source: "inline",
      systemPrompt: spec.instructions?.trim() || DEFAULT_INSTRUCTIONS,
      tools: spec.tools,
      model: spec.model,
    },
  };
}

function extractAssistantText(message: any): string {
  if (message?.role !== "assistant" || !Array.isArray(message.content)) return "";
  return message.content
    .filter((part: any) => part?.type === "text" && typeof part.text === "string")
    .map((part: any) => part.text)
    .join("\n");
}

async function runAgent(options: {
  cwd: string;
  parentModel?: string;
  thinkingLevel?: string;
  agents: AgentConfig[];
  spec: AgentSpec;
  step?: number;
  signal?: AbortSignal;
  context: ExtensionContext;
  onUpdate?: (result: AgentResult) => void;
}): Promise<AgentResult> {
  const resolved = resolveAgent(options.spec, options.agents);
  if (!resolved.ok) {
    return {
      agent: options.spec.label || options.spec.agent || "ad-hoc",
      agentSource: "unknown",
      task: options.spec.task,
      exitCode: 1,
      stderr: resolved.error,
      usage: emptyUsage(),
      messages: [],
      output: "",
      errorMessage: resolved.error,
      step: options.step,
    };
  }
  const agent = resolved.agent;

  const args = ["--mode", "rpc", "--no-session", ...getConsumedPluginArgs()];
  args.push(options.context.isProjectTrusted() ? "--approve" : "--no-approve");
  args.push("--exclude-tools", "subagent");
  const model = agent.model ?? options.parentModel;
  if (model) args.push("--model", model);
  if (!agent.model && options.thinkingLevel) args.push("--thinking", options.thinkingLevel);
  if (agent.tools?.length) args.push("--tools", agent.tools.join(","));

  const current: AgentResult = {
    agent: agent.name,
    agentSource: agent.source,
    task: options.spec.task,
    exitCode: -1,
    stderr: "",
    usage: emptyUsage(),
    messages: [],
    output: "",
    model,
    step: options.step,
  };

  const tmpDir = await fs.promises.mkdtemp(path.join(os.tmpdir(), "pi-subagent-"));
  const emit = () => options.onUpdate?.({ ...current, messages: [...current.messages] });
  const taskOwner = childTaskOwner(backgroundTaskOwner(), `sub-${crypto.randomUUID().slice(0, 8)}`);

  try {
    if (agent.systemPrompt) {
      const promptPath = path.join(tmpDir, "system.md");
      await fs.promises.writeFile(promptPath, agent.systemPrompt, { encoding: "utf-8", mode: 0o600 });
      args.push("--append-system-prompt", promptPath);
    }
    let aborted = false;
    const exitCode = await new Promise<number>((resolve) => {
      const invocation = getPiInvocation(args);
      const proc = spawn(invocation.command, invocation.args, {
        cwd: options.cwd,
        env: subagentEnvironment(process.env, taskOwner),
        shell: false,
        stdio: ["pipe", "pipe", "pipe"],
      });
      const decoder = new StringDecoder("utf-8");
      let buffer = "";
      let finished = false;
      let nextRequestId = 0;
      let killTimer: ReturnType<typeof setTimeout> | undefined;
      let forceKillTimer: ReturnType<typeof setTimeout> | undefined;
      let resolveSettled!: () => void;
      const settled = new Promise<void>((resolveSettledEvent) => {
        resolveSettled = resolveSettledEvent;
      });
      const childUiAbort = new AbortController();
      const pending = new Map<
        string,
        { resolve: (response: Record<string, any>) => void; reject: (error: Error) => void }
      >();
      let uiQueue = Promise.resolve();

      const sendRecord = (record: unknown): Promise<void> => {
        const stdin = proc.stdin;
        if (!stdin || stdin.destroyed) return Promise.reject(new Error("Subagent RPC stdin is closed"));
        return new Promise((writeResolve, writeReject) => {
          stdin.write(`${JSON.stringify(record)}\n`, (error) => {
            if (error) writeReject(error);
            else writeResolve();
          });
        });
      };

      const request = (type: string, fields: Record<string, unknown> = {}): Promise<Record<string, any>> => {
        const id = `smelt-subagent-${++nextRequestId}`;
        const response = new Promise<Record<string, any>>((requestResolve, requestReject) => {
          pending.set(id, { resolve: requestResolve, reject: requestReject });
        });
        void sendRecord({ id, type, ...fields }).catch((error: unknown) => {
          const waiting = pending.get(id);
          pending.delete(id);
          waiting?.reject(error instanceof Error ? error : new Error(String(error)));
        });
        return response;
      };

      const cleanup = () => {
        if (finished) return;
        finished = true;
        childUiAbort.abort();
        if (killTimer) clearTimeout(killTimer);
        if (forceKillTimer) clearTimeout(forceKillTimer);
        options.signal?.removeEventListener("abort", abortChild);
        for (const request of pending.values()) {
          request.reject(new Error("Subagent RPC process exited before responding"));
        }
        pending.clear();
      };

      const abortChild = () => {
        aborted = true;
        childUiAbort.abort();
        void sendRecord({ type: "abort" }).catch(() => proc.kill("SIGTERM"));
        killTimer = setTimeout(() => {
          if (finished) return;
          proc.kill("SIGTERM");
          forceKillTimer = setTimeout(() => {
            if (!finished) proc.kill("SIGKILL");
          }, 5000);
        }, 5000);
      };

      const processLine = (line: string) => {
        if (!line.trim()) return;
        let event: any;
        try {
          event = JSON.parse(line);
        } catch {
          return;
        }

        if (event.type === "response" && typeof event.id === "string") {
          const request = pending.get(event.id);
          if (request) {
            pending.delete(event.id);
            request.resolve(event);
          }
          return;
        }
        if (event.type === "extension_ui_request") {
          uiQueue = uiQueue
            .then(() =>
              bridgeSubagentUiRequest(
                event as RpcExtensionUIRequest,
                options.context,
                childUiAbort.signal,
                async (response: RpcExtensionUIResponse) => sendRecord(response),
              ),
            )
            .catch((error: unknown) => {
              current.stderr += `Subagent UI bridge failed: ${error instanceof Error ? error.message : String(error)}\n`;
            });
          return;
        }
        if (event.type === "agent_settled") {
          resolveSettled();
          return;
        }
        if (event.type === "tool_execution_start") {
          emit();
          return;
        }
        if (event.type !== "message_end" || event.message?.role !== "assistant") return;
        current.messages.push(event.message);
        current.usage.turns += 1;
        const usage = event.message.usage;
        if (usage) {
          current.usage.input += usage.input || 0;
          current.usage.output += usage.output || 0;
          current.usage.cacheRead += usage.cacheRead || 0;
          current.usage.cacheWrite += usage.cacheWrite || 0;
          current.usage.cost += usage.cost?.total || 0;
          current.usage.contextTokens = usage.totalTokens || current.usage.contextTokens;
        }
        if (!current.model && event.message.model) current.model = event.message.model;
        if (event.message.stopReason) current.stopReason = event.message.stopReason;
        if (event.message.errorMessage) current.errorMessage = event.message.errorMessage;
        const output = extractAssistantText(event.message);
        if (output) current.output = output;
        emit();
      };

      const consume = (text: string) => {
        buffer += text;
        const lines = buffer.split("\n");
        buffer = lines.pop() || "";
        for (const line of lines) processLine(line);
      };

      proc.stdout.on("data", (chunk) => consume(decoder.write(chunk)));
      proc.stderr.on("data", (chunk) => {
        current.stderr += chunk.toString();
      });
      proc.on("close", (code) => {
        consume(decoder.end());
        if (buffer.trim()) processLine(buffer);
        cleanup();
        if (code !== 0 && !current.errorMessage && !aborted) {
          current.errorMessage = `Subagent RPC process exited with code ${code ?? "unknown"}`;
        }
        resolve(code ?? 1);
      });
      proc.on("error", (error) => {
        cleanup();
        current.errorMessage = error.message;
        resolve(1);
      });

      if (options.signal) {
        if (options.signal.aborted) abortChild();
        else options.signal.addEventListener("abort", abortChild, { once: true });
      }

      void (async () => {
        try {
          if (options.signal?.aborted) throw new Error("Subagent was aborted");
          const state = await request("get_state");
          if (!state.success) throw new Error(state.error || "Could not initialize subagent RPC session");
          if (options.signal?.aborted) throw new Error("Subagent was aborted");
          const prompt = await request("prompt", { message: options.spec.task });
          if (!prompt.success) throw new Error(prompt.error || "Subagent prompt was rejected");
          await settled;
          await uiQueue;
          if (!finished) proc.stdin?.end();
        } catch (error) {
          if (!finished) {
            current.errorMessage = error instanceof Error ? error.message : String(error);
            proc.kill("SIGTERM");
            killTimer = setTimeout(() => {
              if (!finished) proc.kill("SIGKILL");
            }, 5000);
          }
        }
      })();
    });

    current.exitCode = exitCode;
    if (aborted) {
      current.stopReason = "aborted";
      current.errorMessage = current.errorMessage || "Subagent was aborted";
    }
    if (!current.output) current.output = current.errorMessage || current.stderr.trim();
    return current;
  } finally {
    // 子进程不管怎么结束（完成、出错、被取消、被杀），它在宿主里留下的任务都要收掉。
    await endBackgroundTaskOwner(taskOwner).catch(() => undefined);
    await fs.promises.rm(tmpDir, { recursive: true, force: true });
  }
}

const TaskItem = Type.Object({
  task: Type.String({ description: "Task to delegate" }),
  agent: Type.Optional(Type.String({ description: "Optional preset name from ~/.pi/agent/agents/*.md" })),
  instructions: Type.Optional(
    Type.String({ description: "Dynamic system prompt for this subagent. Prefer this over named presets." }),
  ),
  tools: Type.Optional(Type.Array(Type.String(), { description: "Tool allowlist, e.g. read, grep, bash" })),
  model: Type.Optional(Type.String({ description: "Optional model override (provider/id)" })),
  label: Type.Optional(Type.String({ description: "Short display name for an ad-hoc agent" })),
  cwd: Type.Optional(Type.String({ description: "Working directory override" })),
});

export const SMELT_SUBAGENT_EXTENSION_NAME = "smelt-subagent";
export const SMELT_SUBAGENT_EXTENSION_PATH = `<inline:${SMELT_SUBAGENT_EXTENSION_NAME}>`;
export const SMELT_SUBAGENT_TOOL_NAME = "subagent";

function registerSmeltSubagentTool(pi: ExtensionAPI): void {
  let tools: ReturnType<ExtensionAPI["getAllTools"]>;
  try {
    tools = pi.getAllTools();
  } catch {
    // Pi binds the registry after extension factories run; retry at session_start.
    return;
  }
  if (tools.some((tool) => tool.name === SMELT_SUBAGENT_TOOL_NAME)) return;

  pi.registerTool({
    name: SMELT_SUBAGENT_TOOL_NAME,
    label: "Subagent",
    description: [
      "Delegate a task to an isolated subagent (separate pi RPC process, separate context).",
      "Child tool calls still go through Smelt's host permission UI.",
      "Prefer generating the agent on the fly with task + instructions + tools.",
      "Named presets are optional shortcuts, not required.",
      "Modes: single {task, instructions?}, parallel {tasks:[...]}, chain {chain:[...]} with {previous} for prior output.",
      "Single and parallel tasks run in the background by default and report back when finished. Set background:false to wait. Chain always waits.",
    ].join(" "),
    promptSnippet: "Spawn an isolated subagent with a dynamically generated role",
    promptGuidelines: [
      "Use subagent for isolated work that would pollute the main context.",
      "Generate the subagent dynamically: pass instructions (role, constraints, output format) and tools. Do not require a named preset.",
      "Use a named agent only as a shortcut when it already matches the role. If the task needs a different specialist, write instructions instead of reusing scout/worker/reviewer.",
      "Use subagent parallel tasks for independent lookups; use chain when a later step needs {previous} output.",
      "Leave background unset so the subagent keeps running after this turn. Set background:false only when the next step needs its output immediately.",
    ],
    parameters: Type.Object({
      agent: Type.Optional(Type.String({ description: "Optional preset name" })),
      task: Type.Optional(Type.String({ description: "Task for single mode" })),
      instructions: Type.Optional(Type.String({ description: "Dynamic system prompt for single mode" })),
      tools: Type.Optional(Type.Array(Type.String(), { description: "Tool allowlist for single mode" })),
      model: Type.Optional(Type.String({ description: "Model override for single mode" })),
      label: Type.Optional(Type.String({ description: "Display name for an ad-hoc single agent" })),
      tasks: Type.Optional(Type.Array(TaskItem, { description: "Parallel tasks" })),
      chain: Type.Optional(Type.Array(TaskItem, { description: "Sequential tasks; use {previous} for prior output" })),
      agentScope: Type.Optional(
        Type.Unsafe<"user" | "project" | "both">({
          type: "string",
          enum: ["user", "project", "both"],
          description: 'Agent directories to load. Default: "user".',
          default: "user",
        }),
      ),
      cwd: Type.Optional(Type.String({ description: "Working directory for single mode" })),
      background: Type.Optional(
        Type.Boolean({
          description: "Run without blocking this turn. Default true. Ignored for chain, which always waits.",
        }),
      ),
    }),

    async execute(toolCallId, params, signal, onUpdate, ctx) {
      const agentScope: AgentScope = params.agentScope ?? "user";
      const discovery = discoverAgents(ctx.cwd, agentScope);
      const catalog = formatAgentCatalog(discovery.agents);
      const hasChain = (params.chain?.length ?? 0) > 0;
      const hasTasks = (params.tasks?.length ?? 0) > 0;
      const hasSingle = Boolean(params.task);
      const modeCount = Number(hasChain) + Number(hasTasks) + Number(hasSingle);
      const parentModel = ctx.model ? `${ctx.model.provider}/${ctx.model.id}` : undefined;
      const thinkingLevel = ctx.thinkingLevel;

      const details = (mode: SubagentDetails["mode"], results: AgentResult[]): SubagentDetails => ({
        mode,
        results,
      });
      const fail = (mode: SubagentDetails["mode"], text: string, results: AgentResult[] = []) => ({
        content: [{ type: "text" as const, text }],
        details: details(mode, results),
        isError: true,
      });

      if (modeCount !== 1) {
        return fail(
          "single",
          `Provide exactly one of task, tasks, or chain. Named presets are optional.\nAvailable presets: ${catalog}`,
        );
      }

      // 转后台之前就拒绝。以前先回「已在后台启动」、过一阵才报超限，模型白等一轮才知道要分批。
      if (params.tasks && params.tasks.length > MAX_PARALLEL_TASKS) {
        return fail(
          "parallel",
          `Too many parallel tasks (${params.tasks.length}). Max is ${MAX_PARALLEL_TASKS}; split them into batches of at most ${MAX_PARALLEL_TASKS}.`,
        );
      }

      if ((agentScope === "project" || agentScope === "both") && ctx.hasUI && !ctx.isProjectTrusted()) {
        const names = new Set<string>();
        if (params.agent) names.add(params.agent);
        for (const item of params.tasks ?? []) if (item.agent) names.add(item.agent);
        for (const item of params.chain ?? []) if (item.agent) names.add(item.agent);
        const projectAgents = discovery.agents.filter((agent) => agent.source === "project" && names.has(agent.name));
        if (projectAgents.length > 0) {
          const ok = await ctx.ui.confirm(
            "Run project-local agents?",
            `Agents: ${projectAgents.map((agent) => agent.name).join(", ")}\nSource: ${discovery.projectAgentsDir}`,
          );
          if (!ok) return fail(hasChain ? "chain" : hasTasks ? "parallel" : "single", "Canceled: project-local agents not approved.");
        }
      }

      let activeSignal = signal;
      const run = (spec: AgentSpec, step: number | undefined, onResult?: (result: AgentResult) => void) =>
        runAgent({
          cwd: spec.cwd || ctx.cwd,
          parentModel,
          thinkingLevel,
          agents: discovery.agents,
          spec,
          step,
          signal: activeSignal,
          context: ctx,
          onUpdate: onResult,
        });

      // 进度交给谁：前台就是这次工具调用；转后台后工具已经返回，改交给宿主里的那条后台任务。
      const finish = async (report: typeof onUpdate = onUpdate) => {
      if (params.chain?.length) {
        const results: AgentResult[] = [];
        let previous = "";
        for (let i = 0; i < params.chain.length; i++) {
          const step = params.chain[i];
          const result = await run(
            { ...step, task: step.task.replaceAll("{previous}", previous) },
            i + 1,
            (partial) => {
              report?.({
                content: [{ type: "text", text: partial.output || `(running ${partial.agent}...)` }],
                details: details("chain", [...results, partial]),
              });
            },
          );
          results.push(result);
          if (failed(result)) {
            return fail(
              "chain",
              `Chain stopped at step ${i + 1} (${step.label || step.agent || "ad-hoc"}): ${result.output || result.stderr || "failed"}`,
              results,
            );
          }
          previous = result.output;
        }
        return {
          content: [{ type: "text" as const, text: cap(results.at(-1)?.output || "(no output)") }],
          details: details("chain", results),
        };
      }

      if (params.tasks?.length) {
        const results: AgentResult[] = params.tasks.map((task) => ({
          agent: task.label || task.agent || "ad-hoc",
          agentSource: task.agent ? "unknown" : "inline",
          task: task.task,
          exitCode: -1,
          stderr: "",
          usage: emptyUsage(),
          messages: [],
          output: "",
        }));
        const emit = () => {
          const running = results.filter((result) => result.exitCode === -1).length;
          const done = results.length - running;
          report?.({
            content: [{ type: "text", text: `Parallel: ${done}/${results.length} done, ${running} running...` }],
            details: details("parallel", [...results]),
          });
        };
        await mapWithConcurrency(params.tasks, MAX_CONCURRENCY, async (task, index) => {
          results[index] = await run(task, undefined, (partial) => {
            results[index] = partial;
            emit();
          });
          emit();
          return results[index];
        });
        const succeeded = results.filter((result) => !failed(result)).length;
        const summaries = results.map((result) => {
          const status = failed(result) ? `failed${result.stopReason ? ` (${result.stopReason})` : ""}` : "completed";
          return `### [${result.agent}] ${status}\n\n${cap(result.output || "(no output)")}`;
        });
        return {
          content: [
            {
              type: "text" as const,
              text: `Parallel: ${succeeded}/${results.length} succeeded\n\n${summaries.join("\n\n---\n\n")}`,
            },
          ],
          details: details("parallel", results),
          isError: succeeded !== results.length,
        };
      }

      const result = await run(
        {
          agent: params.agent,
          task: params.task!,
          instructions: params.instructions,
          tools: params.tools,
          model: params.model,
          label: params.label,
          cwd: params.cwd,
        },
        undefined,
        (partial) => {
          report?.({
            content: [{ type: "text", text: partial.output || `(running ${partial.agent}...)` }],
            details: details("single", [partial]),
          });
        },
      );
      if (failed(result)) {
        return fail("single", `Agent ${result.stopReason || "failed"}: ${result.output || result.stderr || "failed"}`, [result]);
      }
      return {
        content: [{ type: "text" as const, text: cap(result.output || "(no output)") }],
        details: details("single", [result]),
      };
      };

      if (subagentShouldBackground(params)) {
        const controller = new AbortController();
        backgroundRuns.add(controller);
        activeSignal = controller.signal;
        const title = hasTasks ? `${params.tasks!.length} 个并行任务` : (params.label ?? "");
        // 登记进宿主的后台任务：界面看得到它还在跑，模型能 list / stop 它，Pi 没了它也跟着结束。
        // 宿主分的任务 id 就是这个子智能体的 id：启动回复、结果消息、background_task 工具都认这一个。
        const hostTask = await attachBackgroundTask(
          { title: title || "后台子智能体", command: backgroundSubagentCommand(params), toolCallId },
          () => controller.abort(),
        );
        // 不在 Smelt 里（没有宿主）时退回本进程内的编号，照常能跑、能交回结果。
        const id = hostTask?.id ?? `sub-${++subagentSeq}`;
        const report: typeof onUpdate = hostTask
          ? (partial) => hostTask.progress(partial.content.map((part) => ("text" in part ? part.text : "")).join("\n"))
          : undefined;
        void finish(report)
          .then((result) => {
            const text = result.content.map((part) => ("text" in part ? part.text : "")).join("\n");
            const resultFailed = "isError" in result && result.isError === true;
            hostTask?.finish(controller.signal.aborted ? "stopped" : resultFailed ? "failed" : "completed");
            try {
              // 自定义消息而不是用户消息：界面画成状态行，模型也知道这是结果不是新要求。
              void pi.sendMessage(
                subagentResultMessage({ id, title, failed: resultFailed, text: cap(text) }),
                { triggerTurn: true, deliverAs: "followUp" },
              );
            } catch {
              // 会话还不能收消息时，子进程的结果留在它自己的记录里。
            }
          })
          .finally(() => {
            // 上面没走到（`finish` 抛错）时也要收掉，不然列表里留一个在跑的任务直到连接断开。
            hostTask?.finish("failed");
            backgroundRuns.delete(controller);
          });
        const stopHint = hostTask ? `要提前停掉，用 ${SMELT_BACKGROUND_TASK_STOP_TOOL_NAME} 停 ${id}。` : "";
        return {
          content: [{ type: "text" as const, text: `已在后台启动子智能体 ${id}。这一轮可以继续，结束后会把结果发回。${stopHint}` }],
          details: details(hasTasks ? "parallel" : "single", []),
        };
      }
      return finish();
    },

  });
}

export default function smeltSubagentExtension(pi: ExtensionAPI): void {
  registerSmeltSubagentTool(pi);
  pi.on("session_start", () => registerSmeltSubagentTool(pi));
  pi.on("session_shutdown", (event) => {
    if (event.reason !== "quit") return;
    for (const controller of backgroundRuns) controller.abort();
    backgroundRuns.clear();
  });
}
