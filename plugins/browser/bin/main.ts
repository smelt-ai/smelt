// Browser is a first-party Shared Bun module. Its package contains no executable: the signed
// Shared Bun Host imports this data file and keeps its state under the plugin's existing directory.

import { readFileSync, mkdirSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const MAX_HISTORY = 200;
const DATA_FILE = "browser.json";
const MANIFEST_URL = new URL("../plugin.json", import.meta.url);

type InvocationRequest = {
  invocation_id: string;
  contribution_id: string;
  operation: string;
  payload: unknown;
  deadline_ms: number;
};

type Bookmark = {
  url: string;
  title: string;
};

type Visit = Bookmark & {
  at: number;
};

type Data = {
  bookmarks: Bookmark[];
  history: Visit[];
};

type SharedPluginContext = {
  pluginId: string;
  dataDir: string;
};

type BrowserState = {
  dataDir: string;
  dataPath: string;
  data: Data;
};

class InvocationFailure extends Error {
  readonly code = "rejected";
  readonly retryable = false;

  constructor(message: string) {
    super(message);
    this.name = "InvocationFailure";
  }
}

const manifest = readManifest();
let state: BrowserState | undefined;

function readManifest(): { id: string; version: string } {
  const value = JSON.parse(readFileSync(MANIFEST_URL, "utf8")) as {
    id?: unknown;
    version?: unknown;
  };
  if (typeof value.id !== "string" || typeof value.version !== "string") {
    throw new Error("browser plugin manifest identity is invalid");
  }
  return { id: value.id, version: value.version };
}

function emptyData(): Data {
  return { bookmarks: [], history: [] };
}

function loadData(path: string): Data {
  try {
    return decodeData(JSON.parse(readFileSync(path, "utf8")));
  } catch {
    return emptyData();
  }
}

function decodeData(value: unknown): Data {
  if (!isRecord(value)) {
    throw new Error("browser state must be an object");
  }
  return {
    bookmarks: value.bookmarks === undefined ? [] : decodeBookmarks(value.bookmarks),
    history: value.history === undefined ? [] : decodeVisits(value.history),
  };
}

function decodeBookmarks(value: unknown): Bookmark[] {
  if (!Array.isArray(value)) {
    throw new Error("browser bookmarks must be an array");
  }
  return value.map((bookmark) => {
    if (
      !isRecord(bookmark) ||
      typeof bookmark.url !== "string" ||
      typeof bookmark.title !== "string"
    ) {
      throw new Error("browser bookmark is invalid");
    }
    return { url: bookmark.url, title: bookmark.title };
  });
}

function decodeVisits(value: unknown): Visit[] {
  if (!Array.isArray(value)) {
    throw new Error("browser history must be an array");
  }
  return value.map((visit) => {
    if (
      !isRecord(visit) ||
      typeof visit.url !== "string" ||
      typeof visit.title !== "string" ||
      !Number.isSafeInteger(visit.at) ||
      visit.at < 0
    ) {
      throw new Error("browser history entry is invalid");
    }
    return { url: visit.url, title: visit.title, at: visit.at };
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stateFor(context: SharedPluginContext): BrowserState {
  if (context.pluginId !== manifest.id) {
    throw new Error("shared bun host supplied a mismatched browser plugin identity");
  }
  if (state === undefined) {
    state = {
      dataDir: context.dataDir,
      dataPath: join(context.dataDir, DATA_FILE),
      data: loadData(join(context.dataDir, DATA_FILE)),
    };
  } else if (state.dataDir !== context.dataDir) {
    throw new Error("shared bun host changed the browser data directory");
  }
  return state;
}

function persist(state: BrowserState): void {
  try {
    mkdirSync(state.dataDir, { recursive: true });
    const temporary = `${state.dataPath}.tmp`;
    writeFileSync(temporary, JSON.stringify(state.data, null, 2));
    renameSync(temporary, state.dataPath);
  } catch (error) {
    throw new InvocationFailure(String(error));
  }
}

function normalizeUrl(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed.length === 0 || Array.from(trimmed).length > 2048) {
    throw new InvocationFailure("URL 为空或过长");
  }
  if (/\p{Cc}/u.test(trimmed)) {
    throw new InvocationFailure("URL 含控制字符");
  }
  const lowered = trimmed.replace(/[A-Z]/g, (character) => character.toLowerCase());
  if (lowered.startsWith("http://") || lowered.startsWith("https://")) {
    return trimmed;
  }
  const authority = trimmed.split(/[/?#]/, 1)[0] ?? trimmed;
  const colon = authority.indexOf(":");
  if (colon >= 0 && !/^[0-9]+$/.test(authority.slice(colon + 1))) {
    throw new InvocationFailure("只支持 http 与 https");
  }
  return `https://${trimmed}`;
}

function clampTitle(title: string, fallback: string): string {
  const source = title.trim() || fallback;
  return Array.from(source)
    .filter((character) => !/\p{Cc}/u.test(character))
    .slice(0, 120)
    .join("");
}

function text(payload: Record<string, unknown>, key: string): string {
  const value = payload[key];
  return typeof value === "string" ? value : "";
}

function handle(state: BrowserState, request: InvocationRequest): Data {
  if (request.operation !== "panel.message") {
    throw new InvocationFailure("unsupported invocation operation");
  }
  if (!isRecord(request.payload)) {
    throw new InvocationFailure("unknown panel op");
  }
  const op = text(request.payload, "op");
  switch (op) {
    case "state":
      break;
    case "bookmark.add": {
      const url = normalizeUrl(text(request.payload, "url"));
      const title = clampTitle(text(request.payload, "title"), url);
      const existing = state.data.bookmarks.find((bookmark) => bookmark.url === url);
      if (existing === undefined) {
        state.data.bookmarks.push({ url, title });
      } else {
        existing.title = title;
      }
      persist(state);
      break;
    }
    case "bookmark.remove":
      state.data.bookmarks = state.data.bookmarks.filter(
        (bookmark) => bookmark.url !== text(request.payload, "url"),
      );
      persist(state);
      break;
    case "history.push": {
      const url = normalizeUrl(text(request.payload, "url"));
      const title = clampTitle(text(request.payload, "title"), url);
      const rawAt = request.payload.at;
      const at =
        typeof rawAt === "number" && Number.isSafeInteger(rawAt) && rawAt >= 0 ? rawAt : 0;
      state.data.history = state.data.history.filter((visit) => visit.url !== url);
      state.data.history.unshift({ url, title, at });
      state.data.history.length = Math.min(state.data.history.length, MAX_HISTORY);
      persist(state);
      break;
    }
    case "history.clear":
      state.data.history = [];
      persist(state);
      break;
    default:
      throw new InvocationFailure("unknown panel op");
  }
  return state.data;
}

const sharedPlugin = {
  invoke(request: InvocationRequest, context: SharedPluginContext) {
    const op = isRecord(request.payload) ? text(request.payload, "op") : "";
    return {
      op,
      state: handle(stateFor(context), request),
      version: manifest.version,
    };
  },
};

export default sharedPlugin;
