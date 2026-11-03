import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { StringDecoder } from "node:string_decoder";
import { Type } from "@earendil-works/pi-ai";
import type {
	ExtensionAPI,
	ExtensionContext,
	RpcExtensionUIRequest,
	RpcExtensionUIResponse,
} from "@earendil-works/pi-coding-agent";
import {
  SMELT_BACKGROUND_TASK_OWNER_ENV,
  backgroundTaskOwner,
  endBackgroundTaskOwner,
} from "../background-task.ts";
import { inheritedMcpSelectionEnvironment } from "../mcp-selection.ts";
import { getConsumedPluginArgs } from "../plugin-args.ts";
import { type AgentConfig, type AgentScope, discoverAgents, formatAgentCatalog } from "./agents.ts";
import { type SubagentDispatch, openSubagentDispatch } from "./host.ts";
import {
  MAX_RUNNING_SUBAGENTS,
  ProcessSlots,
  appendTail,
  subagentWrapUpMessage,
  throttleLatest,
  turnLimitAction,
  turnLimitMessage,
} from "./limits.ts";
import { type SubagentResultStatus, type SubagentRunStatus, subagentResultMessage } from "./result.ts";
import { bridgeSubagentUiRequest } from "./ui-bridge.ts";

const MAX_PARALLEL_TASKS = 8;
const OUTPUT_CAP = 32_000;
/** 「正在做什么」最多这么长。 */
const ACTIVITY_CHARS = 120;
/** 本进程所有子智能体共用的进程名额（前台、后台、并行、链式都算）。 */
export const subagentSlots = new ProcessSlots(MAX_RUNNING_SUBAGENTS);
const backgroundRuns = new Set<AbortController>();
/** 本进程里还在跑的子智能体，按 id 找到它好停下。界面经宿主停、模型用 `subagent_stop` 停都走这里。 */
const runningSubagents = new Map<string, AbortController>();
let subagentSeq = 0;
/**
 * 当前会话的 ctx。后台子智能体会跨过回退（fork）继续跑，派它时捕获的 ctx 随旧会话作废；
 * 子进程要弹审批时用这一个，不然读作废的 ctx 会抛错，子进程一直等不到答复。
 */
let liveContext: ExtensionContext | undefined;

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
  /** 来自智能体定义的 `max_turns`。临时写的子智能体没有，不限轮数。 */
  maxTurns?: number;
};

const DEFAULT_INSTRUCTIONS = [
  "You are an isolated subagent. Complete the assigned task autonomously.",
  "Return a concise result the parent agent can use without re-doing your work.",
  "Do not ask the parent to continue your tool calls.",
].join("\n");

type AgentResult = {
  /** 子智能体 id。宿主分的；不在 Smelt 里时是本进程编号。 */
  id: string;
  title: string;
  agent: string;
  agentSource: AgentSource;
  task: string;
  /** -1 表示还在跑。 */
  exitCode: number;
  stderr: string;
  usage: Usage;
  toolCalls: number;
  /** 最近一次工具调用的标题。 */
  lastActivity?: string;
  output: string;
  model?: string;
  stopReason?: string;
  errorMessage?: string;
  step?: number;
  /** 被叫停（界面、`subagent_stop`、父回合被取消），不是自己出错。 */
  stopped?: boolean;
};

type SubagentMode = "single" | "parallel" | "chain";

/** 工具结果里记下的一个子智能体。过程在它的子会话里，这里只有结果。 */
type SubagentRunDetails = {
  id: string;
  title: string;
  agent: string;
  model?: string;
  status: SubagentRunStatus;
  usage: Usage;
  output: string;
};

/** 写进父会话的 details。Smelt 按 `runs` 画子智能体卡片，按 id 打开子会话。 */
export type SubagentDetails = {
  mode: SubagentMode;
  background: boolean;
  runs: SubagentRunDetails[];
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

export function runStatus(result: Pick<AgentResult, "exitCode" | "stopped" | "stopReason">): SubagentRunStatus {
  if (result.exitCode === -1) return "running";
  if (result.stopped) return "stopped";
  return failed(result as AgentResult) ? "failed" : "completed";
}

function cap(text: string): string {
  if (text.length <= OUTPUT_CAP) return text;
  return `${text.slice(0, OUTPUT_CAP)}\n\n[truncated ${text.length - OUTPUT_CAP} chars]`;
}

/**
 * 写进父会话的 details：每个子智能体只留 id、标题、状态、用量和结果（截断）。子会话的
 * 消息、思考、stderr 都不进父会话；出错时 stderr 的结尾已经在输出里。
 */
export function subagentDetails(mode: SubagentMode, background: boolean, results: AgentResult[]): SubagentDetails {
  return {
    mode,
    background,
    runs: results.map((result) => {
      const status = runStatus(result);
      return {
        id: result.id,
        title: result.title,
        agent: result.agent,
        ...(result.model ? { model: result.model } : {}),
        status,
        usage: result.usage,
        output: status === "running" ? "" : cap(result.output),
      };
    }),
  };
}

/** 一次派发交回会话时的状态：全完成是完成，全停掉是停止，其余有失败的是失败。 */
export function dispatchStatus(results: AgentResult[]): SubagentResultStatus {
  const statuses = results.map(runStatus);
  if (statuses.length > 0 && statuses.every((status) => status === "stopped")) return "stopped";
  if (statuses.some((status) => status !== "completed" && status !== "stopped")) return "failed";
  return "completed";
}

/**
 * 子智能体在会话宿主里的归属：父归属下的一个新分支。子进程照样用宿主跑命令（长命令也能
 * 转后台），但它的任务和完成通知都归它自己，不进父会话。
 */
export function childTaskOwner(parentOwner: string, runId: string): string {
  return `${parentOwner}/${runId}`;
}

/** 子进程的环境：父进程的环境，加上父进程启动时消费掉的 MCP 勾选，和它在宿主里的归属。 */
export function subagentEnvironment(environment: NodeJS.ProcessEnv, owner: string): NodeJS.ProcessEnv {
  return { ...environment, ...inheritedMcpSelectionEnvironment(), [SMELT_BACKGROUND_TASK_OWNER_ENV]: owner };
}

/** 子进程的会话参数：宿主给了子会话文件就落在那里，没有（不在 Smelt 里）就不落盘。 */
export function subagentSessionArgs(sessionFile: string | undefined): string[] {
  return sessionFile ? ["--session", sessionFile] : ["--no-session"];
}

/** 一个子智能体在界面上叫什么：标签、智能体名，都没有就是任务的第一行。 */
export function subagentTitle(spec: Pick<AgentSpec, "label" | "agent" | "task">): string {
  const named = spec.label?.trim() || spec.agent?.trim();
  if (named) return named;
  const firstLine = spec.task.trim().split("\n")[0] ?? "";
  return firstLine.length > 60 ? `${firstLine.slice(0, 60)}…` : firstLine || "子智能体";
}

/** 一次派发在界面上叫什么。 */
export function dispatchTitle(params: {
  task?: string;
  label?: string;
  agent?: string;
  tasks?: ReadonlyArray<unknown>;
  chain?: ReadonlyArray<unknown>;
}): string {
  if (params.tasks?.length) return `${params.tasks.length} 个并行任务`;
  if (params.chain?.length) return `${params.chain.length} 步串联`;
  return subagentTitle({ task: params.task ?? "", label: params.label, agent: params.agent });
}

/** 子智能体正在调的工具，写成「正在做什么」：工具名加命令、路径或查询。 */
export function subagentActivity(toolName: unknown, args: unknown): string | undefined {
  if (typeof toolName !== "string" || !toolName) return undefined;
  const row = args && typeof args === "object" ? (args as Record<string, unknown>) : {};
  const detail = ["command", "path", "file_path", "pattern", "query", "url"]
    .map((key) => row[key])
    .find((value): value is string => typeof value === "string" && value.trim() !== "");
  const text = detail ? `${toolName} ${detail.replace(/\s+/g, " ").trim()}` : toolName;
  return text.length > ACTIVITY_CHARS ? `${text.slice(0, ACTIVITY_CHARS)}…` : text;
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
        maxTurns: preset.maxTurns,
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

function pendingResult(id: string, spec: AgentSpec, step?: number): AgentResult {
  return {
    id,
    title: subagentTitle(spec),
    agent: spec.label || spec.agent || "ad-hoc",
    agentSource: spec.agent ? "unknown" : "inline",
    task: spec.task,
    exitCode: -1,
    stderr: "",
    usage: emptyUsage(),
    toolCalls: 0,
    output: "",
    model: spec.model,
    step,
  };
}

export async function runAgent(options: {
  cwd: string;
  parentModel?: string;
  thinkingLevel?: string;
  agents: AgentConfig[];
  spec: AgentSpec;
  step?: number;
  signal?: AbortSignal;
  context: ExtensionContext;
  /** 子智能体 id。不给就用本进程编号（测试、不在 Smelt 里）。 */
  id?: string;
  /** 宿主给的子会话文件。不给就不落盘。 */
  sessionFile?: string;
  onUpdate?: (result: AgentResult) => void;
  /** 怎么起子 Pi 进程；默认就是本进程的入口。测试换成假的 RPC 子进程。 */
  piInvocation?: (args: string[]) => { command: string; args: string[] };
}): Promise<AgentResult> {
  const id = options.id ?? `sub-${++subagentSeq}`;
  const resolved = resolveAgent(options.spec, options.agents);
  if (!resolved.ok) {
    return {
      ...pendingResult(id, options.spec, options.step),
      agentSource: "unknown",
      exitCode: 1,
      stderr: resolved.error,
      errorMessage: resolved.error,
    };
  }
  const agent = resolved.agent;

  // 排队等进程名额。排队时被叫停就不起进程。
  const release = await subagentSlots.acquire(options.signal);
  if (!release) {
    return {
      ...pendingResult(id, options.spec, options.step),
      agent: agent.name,
      agentSource: agent.source,
      exitCode: 1,
      output: "Subagent was aborted",
      stopReason: "aborted",
      errorMessage: "Subagent was aborted",
      stopped: true,
    };
  }
  try {
    return await runAgentProcess(options, id, agent);
  } finally {
    release();
  }
}

async function runAgentProcess(
  options: Parameters<typeof runAgent>[0],
  id: string,
  agent: ResolvedAgent,
): Promise<AgentResult> {
  const args = ["--mode", "rpc", ...subagentSessionArgs(options.sessionFile), ...getConsumedPluginArgs()];
  args.push(options.context.isProjectTrusted() ? "--approve" : "--no-approve");
  args.push("--exclude-tools", "subagent");
  const model = agent.model ?? options.parentModel;
  if (model) args.push("--model", model);
  if (!agent.model && options.thinkingLevel) args.push("--thinking", options.thinkingLevel);
  if (agent.tools?.length) args.push("--tools", agent.tools.join(","));

  const current: AgentResult = {
    ...pendingResult(id, options.spec, options.step),
    agent: agent.name,
    agentSource: agent.source,
    model,
  };

  const tmpDir = await fs.promises.mkdtemp(path.join(os.tmpdir(), "pi-subagent-"));
  const emit = () => options.onUpdate?.({ ...current, usage: { ...current.usage } });
  const taskOwner = childTaskOwner(backgroundTaskOwner(), `sub-${crypto.randomUUID().slice(0, 8)}`);

  try {
    if (agent.systemPrompt) {
      const promptPath = path.join(tmpDir, "system.md");
      await fs.promises.writeFile(promptPath, agent.systemPrompt, { encoding: "utf-8", mode: 0o600 });
      args.push("--append-system-prompt", promptPath);
    }
    let aborted = false;
    let turnLimited = false;
    const exitCode = await new Promise<number>((resolve) => {
      const invocation = (options.piInvocation ?? getPiInvocation)(args);
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
        const requestId = `smelt-subagent-${++nextRequestId}`;
        const response = new Promise<Record<string, any>>((requestResolve, requestReject) => {
          pending.set(requestId, { resolve: requestResolve, reject: requestReject });
        });
        void sendRecord({ id: requestId, type, ...fields }).catch((error: unknown) => {
          const waiting = pending.get(requestId);
          pending.delete(requestId);
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
                liveContext ?? options.context,
                childUiAbort.signal,
                async (response: RpcExtensionUIResponse) => sendRecord(response),
              ),
            )
            .catch((error: unknown) => {
              current.stderr = appendTail(
                current.stderr,
                `Subagent UI bridge failed: ${error instanceof Error ? error.message : String(error)}\n`,
              );
            });
          return;
        }
        if (event.type === "agent_settled") {
          resolveSettled();
          return;
        }
        if (event.type === "tool_execution_start") {
          current.toolCalls += 1;
          current.lastActivity = subagentActivity(event.toolName, event.args) ?? current.lastActivity;
          emit();
          return;
        }
        if (event.type !== "message_end" || event.message?.role !== "assistant") return;
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
        // 轮数上限：先插话要它收尾，再不停就中止。只看还要接着调工具的消息，收尾的那条不用管。
        const action = turnLimitAction(
          current.usage.turns,
          event.message.stopReason === "toolUse",
          agent.maxTurns,
        );
        if (action === "wrap_up" && agent.maxTurns !== undefined) {
          void request("steer", { message: subagentWrapUpMessage(agent.maxTurns) }).catch(() => undefined);
        } else if (action === "abort" && !turnLimited) {
          turnLimited = true;
          abortChild();
        }
      };

      const consume = (text: string) => {
        buffer += text;
        const lines = buffer.split("\n");
        buffer = lines.pop() || "";
        for (const line of lines) processLine(line);
      };

      proc.stdout.on("data", (chunk) => consume(decoder.write(chunk)));
      proc.stderr.on("data", (chunk) => {
        current.stderr = appendTail(current.stderr, chunk.toString());
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
    // 中止不是轮数上限触发的，就是被叫停：界面、`subagent_stop`、父回合被取消。
    if (aborted && !turnLimited && options.signal?.aborted) current.stopped = true;
    if (turnLimited && agent.maxTurns !== undefined) {
      current.errorMessage = turnLimitMessage(current.usage.turns, agent.maxTurns);
      current.output = current.output ? `${current.errorMessage}\n\n${current.output}` : current.errorMessage;
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
export const SMELT_SUBAGENT_STOP_TOOL_NAME = "subagent_stop";

/**
 * 一个要跑的子智能体：登记好的 id 和子会话文件，自己的停止开关（跟着这次派发的总开关），
 * 以及把进展报给宿主的限频出口。
 */
type PreparedRun = {
  spec: AgentSpec;
  step?: number;
  id: string;
  sessionFile?: string;
  controller: AbortController;
  /** 登记失败时的原因。这个子智能体不跑，直接算失败。 */
  error?: string;
};

/** 停掉本进程里在跑的子智能体。`ids` 为空就停全部。返回停了哪些、哪些不认识。 */
export function stopSubagents(ids: readonly string[] | undefined): { stopped: string[]; unknown: string[] } {
  const targets = ids?.length ? ids : [...runningSubagents.keys()];
  const stopped: string[] = [];
  const unknown: string[] = [];
  for (const id of targets) {
    const controller = runningSubagents.get(id);
    if (!controller) {
      unknown.push(id);
      continue;
    }
    controller.abort();
    stopped.push(id);
  }
  return { stopped, unknown };
}

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
      `Use ${SMELT_SUBAGENT_STOP_TOOL_NAME} with the subagent ids to stop subagents that are no longer needed.`,
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
      liveContext = ctx;
      const agentScope: AgentScope = params.agentScope ?? "user";
      const discovery = discoverAgents(ctx.cwd, agentScope);
      const catalog = formatAgentCatalog(discovery.agents);
      const hasChain = (params.chain?.length ?? 0) > 0;
      const hasTasks = (params.tasks?.length ?? 0) > 0;
      const hasSingle = Boolean(params.task);
      const modeCount = Number(hasChain) + Number(hasTasks) + Number(hasSingle);
      const mode: SubagentMode = hasChain ? "chain" : hasTasks ? "parallel" : "single";
      const parentModel = ctx.model ? `${ctx.model.provider}/${ctx.model.id}` : undefined;
      const thinkingLevel = ctx.thinkingLevel;

      const fail = (text: string) => ({
        content: [{ type: "text" as const, text }],
        details: subagentDetails(mode, false, []),
        isError: true,
      });

      if (modeCount !== 1) {
        return fail(`Provide exactly one of task, tasks, or chain. Named presets are optional.\nAvailable presets: ${catalog}`);
      }

      // 转后台之前就拒绝。以前先回「已在后台启动」、过一阵才报超限，模型白等一轮才知道要分批。
      if (params.tasks && params.tasks.length > MAX_PARALLEL_TASKS) {
        return fail(
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
          if (!ok) return fail("Canceled: project-local agents not approved.");
        }
      }

      const background = subagentShouldBackground(params);
      // 后台派发不跟这次工具调用的取消走：工具一返回，这个 signal 就不再管事。
      const dispatchController = new AbortController();
      if (!background) {
        if (signal?.aborted) dispatchController.abort();
        else signal?.addEventListener("abort", () => dispatchController.abort(), { once: true });
      }
      const dispatch: SubagentDispatch | undefined = await openSubagentDispatch(
        { toolCallId, background, title: dispatchTitle(params) },
        (id) => runningSubagents.get(id)?.abort(),
      );
      const parentSessionId = sessionIdOf(ctx);

      /** 登记一个子智能体，拿到 id 和子会话文件。它自己的停止开关跟着派发的总开关。 */
      const prepare = async (spec: AgentSpec, step?: number): Promise<PreparedRun> => {
        const controller = new AbortController();
        if (dispatchController.signal.aborted) controller.abort();
        else dispatchController.signal.addEventListener("abort", () => controller.abort(), { once: true });
        let id = `sub-${++subagentSeq}`;
        let sessionFile: string | undefined;
        let error: string | undefined;
        if (dispatch) {
          try {
            const registered = await dispatch.register({
              parentSessionId,
              title: subagentTitle(spec),
              agent: spec.agent || spec.label || "ad-hoc",
              ...(spec.model ? { model: spec.model } : {}),
            });
            id = registered.id;
            sessionFile = registered.sessionFile;
          } catch (registerError) {
            error = registerError instanceof Error ? registerError.message : String(registerError);
          }
        }
        runningSubagents.set(id, controller);
        return { spec, step, id, sessionFile, controller, error };
      };

      /** 跑一个登记好的子智能体：进展限频报给宿主，结束时报一次，交回最终结果。 */
      const run = async (prepared: PreparedRun, onResult?: (result: AgentResult) => void): Promise<AgentResult> => {
        const toHost = dispatch
          ? throttleLatest((partial: AgentResult) =>
              dispatch.progress(prepared.id, {
                turns: partial.usage.turns,
                toolCalls: partial.toolCalls,
                ...(partial.lastActivity ? { lastActivity: partial.lastActivity } : {}),
              }),
            )
          : undefined;
        try {
          const result = prepared.error
            ? {
                ...pendingResult(prepared.id, prepared.spec, prepared.step),
                exitCode: 1,
                output: prepared.error,
                errorMessage: prepared.error,
              }
            : await runAgent({
                cwd: prepared.spec.cwd || ctx.cwd,
                parentModel,
                thinkingLevel,
                agents: discovery.agents,
                spec: prepared.spec,
                step: prepared.step,
                signal: prepared.controller.signal,
                context: ctx,
                id: prepared.id,
                sessionFile: prepared.sessionFile,
                onUpdate: (partial) => {
                  toHost?.push(partial);
                  onResult?.(partial);
                },
              });
          toHost?.cancel();
          dispatch?.end(prepared.id, endStatus(result));
          return result;
        } finally {
          toHost?.cancel();
          runningSubagents.delete(prepared.id);
        }
      };

      // 前台进度交给这次工具调用；转后台后工具已经返回，进展只报给宿主。
      // 只送最新一份、限频：结果交出后丢掉还没送的那份。
      const toTool = !background && onUpdate ? throttleLatest(onUpdate) : undefined;
      const report = (text: string, results: AgentResult[]) =>
        toTool?.push({ content: [{ type: "text", text }], details: subagentDetails(mode, background, results) });

      const settle = async (preparedUpfront: PreparedRun[]) => {
        if (params.chain?.length) {
          const results: AgentResult[] = [];
          let previous = "";
          for (let i = 0; i < params.chain.length; i++) {
            const step = params.chain[i]!;
            if (dispatchController.signal.aborted) break;
            const prepared = await prepare({ ...step, task: step.task.replaceAll("{previous}", previous) }, i + 1);
            const result = await run(prepared, (partial) => {
              report(partial.output || `(running ${partial.agent}...)`, [...results, partial]);
            });
            results.push(result);
            if (failed(result)) {
              return {
                text: `Chain stopped at step ${i + 1} (${step.label || step.agent || "ad-hoc"}): ${result.output || result.stderr || "failed"}`,
                results,
                isError: true,
              };
            }
            previous = result.output;
          }
          return { text: cap(results.at(-1)?.output || "(no output)"), results, isError: false };
        }

        const results = preparedUpfront.map((prepared) => pendingResult(prepared.id, prepared.spec, prepared.step));
        if (mode === "parallel") {
          const progressText = () => {
            const running = results.filter((result) => result.exitCode === -1).length;
            return `Parallel: ${results.length - running}/${results.length} done, ${running} running...`;
          };
          // 同时跑几个由进程名额管，这里全部交出去排队。
          await Promise.all(
            preparedUpfront.map(async (prepared, index) => {
              results[index] = await run(prepared, (partial) => {
                results[index] = partial;
                report(progressText(), results);
              });
              report(progressText(), results);
            }),
          );
          const succeeded = results.filter((result) => !failed(result)).length;
          const summaries = results.map((result) => {
            const status = runStatus(result);
            const reason = status === "failed" && result.stopReason ? ` (${result.stopReason})` : "";
            return `### [${result.title}] ${result.id} ${status}${reason}\n\n${cap(result.output || "(no output)")}`;
          });
          return {
            text: `Parallel: ${succeeded}/${results.length} succeeded\n\n${summaries.join("\n\n---\n\n")}`,
            results,
            isError: succeeded !== results.length,
          };
        }

        const result = await run(preparedUpfront[0]!, (partial) => {
          results[0] = partial;
          report(partial.output || `(running ${partial.agent}...)`, results);
        });
        if (failed(result)) {
          return {
            text: `Agent ${result.stopReason || "failed"}: ${result.output || result.stderr || "failed"}`,
            results: [result],
            isError: true,
          };
        }
        return { text: cap(result.output || "(no output)"), results: [result], isError: false };
      };

      // 单个和并行在开跑前全部登记好：后台派发立刻返回时要把每个子智能体的 id 带上。
      const specs: AgentSpec[] = params.tasks?.length
        ? params.tasks
        : params.chain?.length
          ? []
          : [
              {
                agent: params.agent,
                task: params.task!,
                instructions: params.instructions,
                tools: params.tools,
                model: params.model,
                label: params.label,
                cwd: params.cwd,
              },
            ];
      const preparedUpfront = await Promise.all(specs.map((spec) => prepare(spec)));

      if (background) {
        backgroundRuns.add(dispatchController);
        const pendingResults = preparedUpfront.map((prepared) => pendingResult(prepared.id, prepared.spec, prepared.step));
        const title = dispatchTitle(params);
        void settle(preparedUpfront)
          .then(({ text, results }) => {
            const output = cap(text);
            if (dispatch) {
              // 交给宿主送回会话：Pi 真交给模型才算送到，停止时被 clear_queue 清掉会重发，
              // 回退（fork）换了会话也交给新会话。这里捕获的 pi 那时已经不能用了。
              dispatch.finish(output);
              return;
            }
            try {
              const status = dispatchStatus(results);
              // 自定义消息而不是用户消息：界面画成状态行，模型也知道这是结果不是新要求。
              // 全被叫停的不叫醒模型。
              pi.sendMessage(
                subagentResultMessage({
                  toolCallId,
                  title,
                  status,
                  text: output,
                  runs: results.map((result) => ({ id: result.id, title: result.title, status: runStatus(result) })),
                }),
                { triggerTurn: status !== "stopped", deliverAs: "followUp" },
              );
            } catch {
              // 会话还不能收消息时，结果就没有地方交了。
            }
          })
          .catch(() => {
            // `settle` 不该抛；真抛了也要把派发收掉，宿主才会把子智能体记成已中断。
          })
          .finally(() => {
            dispatch?.finish();
            backgroundRuns.delete(dispatchController);
          });
        const ids = pendingResults.map((result) => result.id).join("、");
        return {
          content: [
            {
              type: "text" as const,
              text: `已在后台启动子智能体 ${ids}。这一轮可以继续，结束后会把结果发回。要提前停掉，用 ${SMELT_SUBAGENT_STOP_TOOL_NAME} 停这些 id。`,
            },
          ],
          details: subagentDetails(mode, true, pendingResults),
        };
      }

      try {
        const { text, results, isError } = await settle(preparedUpfront);
        return {
          content: [{ type: "text" as const, text }],
          details: subagentDetails(mode, false, results),
          ...(isError ? { isError: true } : {}),
        };
      } finally {
        toTool?.cancel();
        dispatch?.finish();
      }
    },
  });
}

function registerSubagentStopTool(pi: ExtensionAPI): void {
  let tools: ReturnType<ExtensionAPI["getAllTools"]>;
  try {
    tools = pi.getAllTools();
  } catch {
    return;
  }
  if (tools.some((tool) => tool.name === SMELT_SUBAGENT_STOP_TOOL_NAME)) return;
  pi.registerTool({
    name: SMELT_SUBAGENT_STOP_TOOL_NAME,
    label: "Stop subagents",
    description: "Stop running subagents started by the subagent tool. Omit ids to stop every running subagent.",
    parameters: Type.Object({
      ids: Type.Optional(Type.Array(Type.String(), { description: "Subagent ids to stop" })),
    }),
    async execute(_toolCallId, params) {
      const { stopped, unknown } = stopSubagents(params.ids);
      const lines = [
        stopped.length ? `正在停止子智能体 ${stopped.join("、")}。` : "没有在跑的子智能体。",
        ...(unknown.length ? [`不认识或已结束：${unknown.join("、")}。`] : []),
      ];
      return {
        content: [{ type: "text" as const, text: lines.join("\n") }],
        details: { stopped, unknown },
      };
    },
  });
}

function endStatus(result: AgentResult): "completed" | "failed" | "stopped" {
  const status = runStatus(result);
  return status === "running" || status === "interrupted" ? "failed" : status;
}

/** 父会话的 Pi 会话 id。宿主据此记下哪份会话记录引用了这些子智能体。 */
function sessionIdOf(ctx: ExtensionContext): string {
  try {
    return ctx.sessionManager?.getSessionId?.() ?? "";
  } catch {
    // 会话换掉后旧 ctx 一碰就抛错。
    return "";
  }
}

export default function smeltSubagentExtension(pi: ExtensionAPI): void {
  registerSmeltSubagentTool(pi);
  registerSubagentStopTool(pi);
  pi.on("session_start", (_event, ctx) => {
    liveContext = ctx;
    registerSmeltSubagentTool(pi);
    registerSubagentStopTool(pi);
  });
  pi.on("session_shutdown", (event) => {
    if (event.reason !== "quit") return;
    for (const controller of backgroundRuns) controller.abort();
    backgroundRuns.clear();
  });
}
