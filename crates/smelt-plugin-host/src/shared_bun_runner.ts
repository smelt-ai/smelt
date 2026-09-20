import * as fs from "node:fs";
import * as readline from "node:readline";
import { pathToFileURL } from "node:url";

const CONTROL_FD_ENV = "SMELT_SHARED_BUN_HOST_FD";
const MAX_LINE_BYTES = 32 * 1024 * 1024;
const VALID_ERROR_CODES = new Set(["invalid_request", "rejected", "conflict", "internal"]);

type InvocationRequest = {
  invocation_id: string;
  contribution_id: string;
  operation: string;
  payload: unknown;
  deadline_ms: number;
};

type InvocationResponse =
  | { status: "success"; invocation_id: string; result: unknown }
  | {
      status: "error";
      invocation_id: string;
      code: string;
      message: string;
      retryable: boolean;
      details?: unknown;
    };

type Plugin = {
  invoke(
    request: InvocationRequest,
    context: { pluginId: string; dataDir: string },
  ): unknown | Promise<unknown>;
};

type LoadedPlugin = {
  plugin: Plugin;
  context: { pluginId: string; dataDir: string };
};

const fd = Number(process.env[CONTROL_FD_ENV]);
if (!Number.isInteger(fd) || fd < 0) {
  throw new Error("shared bun host control FD is unavailable");
}
delete process.env[CONTROL_FD_ENV];

const input = fs.createReadStream("", { fd, autoClose: false });
const output = fs.createWriteStream("", { fd, autoClose: false });
const reader = readline.createInterface({ input, crlfDelay: Infinity });
const plugins = new Map<string, LoadedPlugin>();

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function invocationResult(value: unknown): unknown {
  if (value === undefined) {
    return null;
  }
  try {
    if (JSON.stringify(value) === undefined) {
      return null;
    }
  } catch (error) {
    throw new Error(`plugin invocation result is not JSON serializable: ${message(error)}`);
  }
  return value;
}

function invocationDetails(value: unknown): unknown | undefined {
  if (value === undefined) {
    return undefined;
  }
  try {
    return JSON.stringify(value) === undefined ? undefined : value;
  } catch {
    return undefined;
  }
}

async function send(value: unknown): Promise<void> {
  const line = JSON.stringify(value);
  if (Buffer.byteLength(line) + 1 > MAX_LINE_BYTES) {
    throw new Error("shared bun host response exceeds line limit");
  }
  await new Promise<void>((resolve, reject) => {
    const onError = (error: Error) => {
      output.off("error", onError);
      reject(error);
    };
    output.once("error", onError);
    if (output.write(`${line}\n`)) {
      output.off("error", onError);
      resolve();
    } else {
      output.once("drain", () => {
        output.off("error", onError);
        resolve();
      });
    }
  });
}

function invocationError(request: InvocationRequest, error: unknown): InvocationResponse {
  if (
    typeof error === "object" &&
    error !== null &&
    (error as { name?: unknown }).name === "InvocationFailure" &&
    typeof (error as { code?: unknown }).code === "string" &&
    VALID_ERROR_CODES.has((error as { code: string }).code)
  ) {
    const failure = error as {
      code: string;
      message?: unknown;
      retryable?: unknown;
      details?: unknown;
    };
    const details = invocationDetails(failure.details);
    return {
      status: "error",
      invocation_id: request.invocation_id,
      code: failure.code,
      message: typeof failure.message === "string" ? failure.message : "plugin rejected invocation",
      retryable: failure.retryable === true,
      ...(details === undefined ? {} : { details }),
    };
  }
  return {
    status: "error",
    invocation_id: request.invocation_id,
    code: "internal",
    message: message(error),
    retryable: false,
  };
}

async function loadPlugins(
  declarations: Array<{ plugin_id: string; entrypoint: string; data_dir: string }>,
): Promise<void> {
  const results: Array<{ plugin_id: string; error?: string }> = [];
  for (const declaration of declarations) {
    try {
      const loaded = await import(pathToFileURL(declaration.entrypoint).href);
      const plugin = loaded.default;
      if (
        typeof plugin !== "object" ||
        plugin === null ||
        typeof (plugin as Partial<Plugin>).invoke !== "function"
      ) {
        throw new Error("shared bun plugin must default-export an object with invoke(request, context)");
      }
      plugins.set(declaration.plugin_id, {
        plugin: plugin as Plugin,
        context: {
          pluginId: declaration.plugin_id,
          dataDir: declaration.data_dir,
        },
      });
      results.push({ plugin_id: declaration.plugin_id });
    } catch (error) {
      results.push({ plugin_id: declaration.plugin_id, error: message(error) });
    }
  }
  await send({ type: "ready", plugins: results });
}

for await (const line of reader) {
  if (Buffer.byteLength(line) > MAX_LINE_BYTES) {
    throw new Error("shared bun host request exceeds line limit");
  }
  const request = JSON.parse(line) as
    | {
        type: "init";
        plugins: Array<{ plugin_id: string; entrypoint: string; data_dir: string }>;
      }
    | { type: "invoke"; plugin_id: string; request: InvocationRequest }
    | { type: "shutdown" };
  switch (request.type) {
    case "init":
      await loadPlugins(request.plugins);
      break;
    case "invoke": {
      const loaded = plugins.get(request.plugin_id);
      const response: InvocationResponse =
        loaded === undefined
          ? {
              status: "error",
              invocation_id: request.request.invocation_id,
              code: "rejected",
              message: "plugin is not loaded in the shared bun host",
              retryable: false,
            }
          : Date.now() > request.request.deadline_ms
            ? {
                status: "error",
                invocation_id: request.request.invocation_id,
                code: "invalid_request",
                message: "invocation deadline has expired",
                retryable: false,
              }
            : await (async () => {
                try {
                  return {
                    status: "success" as const,
                    invocation_id: request.request.invocation_id,
                    result: invocationResult(
                      await loaded.plugin.invoke(request.request, loaded.context),
                    ),
                  };
                } catch (error) {
                  return invocationError(request.request, error);
                }
              })();
      await send({ type: "invocation_result", plugin_id: request.plugin_id, response });
      break;
    }
    case "shutdown":
      reader.close();
      process.exit(0);
  }
}
