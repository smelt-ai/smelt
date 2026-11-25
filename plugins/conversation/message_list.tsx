// 对话消息流列表：自然时间序虚拟列表（initialScrollAtEnd + maintainVisibleContentPosition 自动粘底，
// 零 inverted 空闲唤醒）、局部流式信号、跨气泡选区、折叠过程行、差异弹窗入口、子智能体卡片与右键菜单。
import { cleanup, computed, Image, List, Scroll, signal, Text, View } from "nsh";
import { MenuItem, MenuSeparator } from "nsh/base";
import {
  clipboardWrite,
  contextMenuItems,
  diffLineStats,
  isInterruptMarker,
  run,
  SmeltButton,
  SmeltContextMenu,
  SmeltDisclosure,
  SmeltDisclosureChevron,
  SmeltIcon,
  SmeltMarkdown,
  theme,
} from "smelt/sdk";
import type { EditDiffDialogState, EditFileItem } from "./edit_diff_dialog.tsx";
import { ct } from "./i18n.ts";
import {
  collectEditFilesFromTool,
  entryUserText,
  formatDuration,
  isUserEntry,
  relativePathToCwd,
  stripCodeFence,
} from "./model.js";

const MESSAGE_MENU = "smelt.conversation/message";

function imageThumbSrc(img: any): string | null {
  if (!img || typeof img !== "object") return null;
  if (typeof img.preview_uri === "string" && img.preview_uri.length > 0) return img.preview_uri;
  if (typeof img.data_b64 === "string" && img.data_b64.length > 0) {
    return `data:${img.mime || "image/png"};base64,${img.data_b64}`;
  }
  return null;
}

function toolIconFor(kind: string): string {
  switch (kind) {
    case "read":
      return "icons/file.svg";
    case "search":
      return "icons/search.svg";
    case "edit":
    case "delete":
    case "move":
      return "icons/pencil.svg";
    case "execute":
      return "icons/terminal.svg";
    case "fetch":
      return "icons/globe.svg";
    case "collaborate":
      return "icons/bot.svg";
    default:
      return "icons/wrench.svg";
  }
}

function summarizeTally(tally: any): string {
  if (!tally || tally.total === 0) return ct("process.working");
  const parts: string[] = [];
  if (tally.reads > 0) parts.push(ct("process.reads", { count: tally.reads }));
  if (tally.searches > 0) parts.push(ct("process.searches", { count: tally.searches }));
  if (tally.edits > 0) parts.push(ct("process.edits", { count: tally.edits }));
  if (tally.commands > 0) parts.push(ct("process.commands", { count: tally.commands }));
  if (tally.web > 0) parts.push(ct("process.web", { count: tally.web }));
  if (tally.subagents > 0) parts.push(ct("process.subagents", { count: tally.subagents }));
  if (parts.length === 0) return ct("process.steps", { count: tally.total });
  return parts.join(" · ");
}

function formatNewSessionTime(atMs: number): string {
  try {
    const d = new Date(atMs);
    const pad = (n: number) => String(n).padStart(2, "0");
    return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  } catch {
    return "";
  }
}

function turnHintMessage(hint: string | null | undefined): string {
  if (!hint) return "";
  switch (hint) {
    case "no_api_key":
      return ct("turn_hint.no_api_key");
    case "api_key_rejected":
      return ct("turn_hint.api_key_rejected");
    case "connection_refused":
      return ct("turn_hint.connection_refused");
    case "rate_limited":
      return ct("turn_hint.rate_limited");
    default:
      return "";
  }
}

function statusLabel(status: string): string {
  switch (status) {
    case "completed":
      return ct("status.completed");
    case "failed":
      return ct("status.failed");
    case "stopped":
      return ct("status.stopped");
    case "timed_out":
      return ct("status.timed_out");
    case "running":
      return ct("status.running");
    case "interrupted":
      return ct("status.interrupted");
    default:
      return status;
  }
}

function DiffFileChips(props: {
  files: () => EditFileItem[];
  cwd: () => string | null;
  machine: () => string | null;
  onOpenDiff: (state: EditDiffDialogState) => void;
}) {
  const look = theme;
  return (
    <If when={props.files().length > 0}>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6, paddingTop: 4 }}>
        <Each of={props.files()}>
          {(file, idx) => (
            <View
              onPress={() =>
                props.onOpenDiff({
                  files: props.files(),
                  selected: idx(),
                  cwd: props.cwd(),
                  machine: props.machine(),
                })
              }
              class="h-6 flex-row items-center gap-1.5 px-2 rounded-md border border-border bg-foreground/[0.06] hover:bg-accent"
            >
              <SmeltIcon path="icons/file.svg" size={11} color={look().palette.muted} />
              <Text
                style={{
                  fontFamily: look().fonts?.mono,
                  fontSize: look().type.text_xsmall.size,
                  color: look().palette.text,
                  numberOfLines: 1,
                }}
              >
                {file().label}
              </Text>
              <If when={file().added > 0}>
                <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.ok }}>
                  +{file().added}
                </Text>
              </If>
              <If when={file().removed > 0}>
                <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                  -{file().removed}
                </Text>
              </If>
            </View>
          )}
        </Each>
      </View>
    </If>
  );
}

function MessageContextWrapper(props: {
  machine: () => string | null;
  sessionId: () => string;
  role: "user" | "assistant";
  globalIndex: () => number;
  copyText: () => string;
  canFork: () => boolean;
  canEditResend: () => boolean;
  onFork: () => void;
  onEditResend: () => void;
  children: any;
}) {
  const look = theme;
  const contributed = signal<any[]>([]);

  const loadContributed = () => {
    contextMenuItems(MESSAGE_MENU, props.machine(), {
      machine: props.machine(),
      session_id: props.sessionId(),
      role: props.role,
      entry_index: props.globalIndex(),
      text: props.copyText(),
    })
      .then((items) => contributed.set(Array.isArray(items) ? items : []))
      .catch(() => contributed.set([]));
  };

  return (
    <SmeltContextMenu
      popupStyle={{ minWidth: 184 }}
      onOpenChange={({ open }: any) => {
        if (open) loadContributed();
      }}
      trigger={props.children}
    >
      <MenuItem
        onSelect={() => {
          const text = props.copyText();
          if (text) void clipboardWrite(text).catch(() => {});
        }}
        style={{ height: 28, paddingHorizontal: 10, borderRadius: 5, flexDirection: "row", alignItems: "center", gap: 8 }}
      >
        <SmeltIcon path="icons/copy.svg" size={13} color={look().palette.muted} />
        <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
          {ct("action.copy")}
        </Text>
      </MenuItem>
      <If when={props.canFork()}>
        <MenuItem
          onSelect={props.onFork}
          style={{ height: 28, paddingHorizontal: 10, borderRadius: 5, flexDirection: "row", alignItems: "center", gap: 8 }}
        >
          <SmeltIcon path="icons/git-branch.svg" size={13} color={look().palette.muted} />
          <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
            {ct("action.fork")}
          </Text>
        </MenuItem>
      </If>
      <If when={props.canEditResend()}>
        <MenuItem
          onSelect={props.onEditResend}
          style={{ height: 28, paddingHorizontal: 10, borderRadius: 5, flexDirection: "row", alignItems: "center", gap: 8 }}
        >
          <SmeltIcon path="icons/pencil.svg" size={13} color={look().palette.muted} />
          <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
            {ct("action.edit_resend")}
          </Text>
        </MenuItem>
      </If>
      <If when={contributed().length > 0}>
        <MenuSeparator />
        <Each of={contributed()}>
          {(item) => (
            <MenuItem
              onSelect={() => {
                const it = item();
                if (it?.command) {
                  void run(it.command, it.args ?? null).catch(() => {});
                }
              }}
              style={{ height: 28, paddingHorizontal: 10, borderRadius: 5, flexDirection: "row", alignItems: "center", gap: 8 }}
            >
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                {item().label ?? ""}
              </Text>
            </MenuItem>
          )}
        </Each>
      </If>
    </SmeltContextMenu>
  );
}

function ToolCallRow(props: {
  tc: () => any;
  cwd: () => string | null;
  machine: () => string | null;
  liveSubagents: () => any[];
  expandedTools: () => Set<string>;
  onToggleTool: (id: string) => void;
  onOpenDiff: (state: EditDiffDialogState) => void;
  onPreviewImage: (img: any) => void;
  onInspectSubagent: (sub: { id: string; title: string }) => void;
  onOpenConversation?: (sid: string) => void;
}) {
  const look = theme;
  const tc = () => props.tc();

  // 1. Pi session_send / session_spawn 事件行
  const peer = computed(() => tc()?.peer ?? null);
  // 2. Pi smelt_ask_user 问答事件行
  const asked = computed(() => tc()?.asked ?? null);
  // 3. 普通工具或子智能体派发
  const editFiles = computed(() => collectEditFilesFromTool(tc(), diffPartStats));
  const textOutputs = computed(() =>
    (tc()?.output ?? [])
      .map((part: any) => {
        if (part && typeof part === "object") {
          if (typeof part.Text === "string") return { kind: "text", text: part.Text };
          if (part.Terminal) {
            return {
              kind: "terminal",
              text: part.Terminal.output ?? "",
              exitCode: part.Terminal.exit_code,
              truncated: Boolean(part.Terminal.truncated),
            };
          }
        }
        return null;
      })
      .filter(Boolean),
  );
  const imageOutputs = computed(() =>
    (tc()?.output ?? [])
      .map((part: any) => (part && typeof part === "object" && part.Image ? part.Image : null))
      .filter(Boolean),
  );
  const hasDetails = computed(
    () =>
      textOutputs().length > 0 ||
      imageOutputs().length > 0 ||
      (tc()?.children?.length ?? 0) > 0,
  );
  const expanded = computed(() => props.expandedTools().has(String(tc()?.id ?? "")));
  const running = computed(() => tc()?.status === "in_progress" || tc()?.status === "pending");
  const failed = computed(() => tc()?.status === "failed");

  return (
    <If
      when={peer() == null && asked() == null}
      else={
        <If
          when={peer() != null}
          else={
            /* smelt_ask_user 问答行 */
            <View
              style={{
                paddingHorizontal: 10,
                paddingVertical: 8,
                borderRadius: look().radius.md,
                borderWidth: 1,
                borderColor: look().palette.hair,
                backgroundColor: look().palette.surface,
                gap: 4,
              }}
            >
              <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
                <SmeltIcon path="icons/info.svg" size={13} color={look().palette.muted} />
                <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                  {asked()?.skipped
                    ? ct("asked.skipped")
                    : (asked()?.answers?.length ?? 0) === 0
                      ? ct("asked.waiting")
                      : tc()?.title || ct("elicit.title")}
                </Text>
              </View>
              <Each of={asked()?.answers ?? []}>
                {(ans) => (
                  <View style={{ paddingLeft: 18, gap: 2 }}>
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                      {ans().question}
                    </Text>
                    <If when={(ans().answer?.length ?? 0) > 0}>
                      <Text style={{ fontSize: look().type.small.size, color: look().palette.text }}>
                        {(ans().answer ?? []).join(", ")}
                      </Text>
                    </If>
                  </View>
                )}
              </Each>
            </View>
          }
        >
          {/* session_send / session_spawn 同伴消息行 */}
          <View
            onPress={() => {
              const target = peer()?.toId ?? peer()?.to_id;
              if (target && props.onOpenConversation) props.onOpenConversation(target);
            }}
            class="hover:bg-accent"
            style={{
              paddingHorizontal: 10,
              paddingVertical: 6,
              borderRadius: look().radius.md,
              borderWidth: 1,
              borderColor: peer()?.error ? look().palette.bad : look().palette.hair,
              backgroundColor: look().palette.surface,
              gap: 3,
            }}
          >
            <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
              <SmeltIcon
                path={peer()?.spawned ? "icons/git-branch.svg" : "icons/arrow-right.svg"}
                size={12}
                color={peer()?.error ? look().palette.bad : look().palette.muted}
              />
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.text }}>
                {ct(peer()?.spawned ? "peer.spawned" : "peer.sent", {
                  to: peer()?.toTitle || peer()?.to_title || peer()?.toId || peer()?.to_id || "",
                })}
              </Text>
            </View>
            <If when={Boolean(peer()?.text)}>
              <Text
                style={{
                  paddingLeft: 18,
                  fontSize: look().type.text_xsmall.size,
                  color: look().palette.muted,
                  numberOfLines: 2,
                }}
              >
                {peer()?.text ?? ""}
              </Text>
            </If>
            <If when={Boolean(peer()?.error)}>
              <Text style={{ paddingLeft: 18, fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                {peer()?.error ?? ""}
              </Text>
            </If>
          </View>
        </If>
      }
    >
      {/* 展开后的文本 / 终端 / 图片 / 嵌套子条目 */}
      <SmeltDisclosure
        open={expanded()}
        style={{ width: "100%" }}
        content={
          <View style={{ paddingLeft: 22, paddingTop: 8, gap: 6 }}>
            <Each of={textOutputs()}>
              {(part: () => any) => (
                <View class="rounded-[10px] border border-border bg-sidebar p-2.5 gap-1">
                  <Scroll style={{ maxHeight: 220 }}>
                    <Text
                      class="font-mono text-muted-foreground"
                      style={{
                        fontSize: look().type.text_xsmall.size,
                        lineHeight: look().type.text_xsmall.line_height,
                      }}
                    >
                      {stripCodeFence(part().text) || ct("tool.no_output")}
                    </Text>
                  </Scroll>
                  <If when={part().kind === "terminal" && (part().truncated || (part().exitCode != null && part().exitCode !== 0))}>
                    <View style={{ flexDirection: "row", gap: 8 }}>
                      <If when={Boolean(part().truncated)}>
                        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                          {ct("tool.truncated")}
                        </Text>
                      </If>
                      <If when={part().exitCode != null && part().exitCode !== 0}>
                        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                          {ct("tool.exit_code", { code: part().exitCode })}
                        </Text>
                      </If>
                    </View>
                  </If>
                </View>
              )}
            </Each>

            <If when={imageOutputs().length > 0}>
              <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
                <Each of={imageOutputs()}>
                  {(img) => {
                    const src = computed(() => imageThumbSrc(img()));
                    return (
                      <If when={src() != null}>
                        <View
                          onPress={() => props.onPreviewImage(img())}
                          style={{
                            borderRadius: 6,
                            borderWidth: 1,
                            borderColor: look().palette.hair,
                            overflow: "hidden",
                            backgroundColor: look().palette.surface,
                          }}
                        >
                          <Image
                            src={src()!}
                            style={{ width: 120, height: 84, objectFit: "contain" }}
                          />
                        </View>
                      </If>
                    );
                  }}
                </Each>
              </View>
            </If>

            <If when={(tc()?.children?.length ?? 0) > 0}>
              <View
                style={{
                  borderLeftWidth: 2,
                  borderLeftColor: look().palette.hair,
                  paddingLeft: 10,
                  gap: 4,
                }}
              >
                <Each of={tc()?.children ?? []}>
                  {(child) => (
                    <View>
                      <If when={Boolean(child()?.Assistant)}>
                        <SmeltMarkdown
                          markdown={child().Assistant.text ?? ""}
                          cwd={props.cwd()}
                          machine={props.machine()}
                          styles={
                            child().Assistant.thought
                              ? { root: { color: look().palette.muted, fontSize: look().type.small.size } }
                              : undefined
                          }
                        />
                      </If>
                      <If when={Boolean(child()?.ToolCall)}>
                        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                          · {child().ToolCall.title}
                        </Text>
                      </If>
                    </View>
                  )}
                </Each>
              </View>
            </If>
          </View>
        }
      >
        <View style={{ width: "100%", gap: 4 }}>
          <View
            onPress={() => {
              if (hasDetails()) props.onToggleTool(String(tc()?.id ?? ""));
            }}
            class={hasDetails() ? "hover:bg-accent" : undefined}
            style={{
              minHeight: 26,
              paddingHorizontal: 8,
              paddingVertical: 4,
              borderRadius: look().radius.row,
              flexDirection: "row",
              alignItems: "center",
              gap: 8,
            }}
          >
            <If
              when={running()}
              else={
                <SmeltIcon
                  path={toolIconFor(tc()?.kind)}
                  size={13}
                  color={failed() ? look().palette.bad : look().palette.muted}
                />
              }
            >
              <SmeltIcon spin size={12} color={look().palette.muted} />
            </If>
            <Text
              style={{
                flex: 1,
                minWidth: 0,
                fontSize: look().type.small.size,
                color: failed() ? look().palette.bad : look().palette.muted,
                numberOfLines: 1,
              }}
            >
              {relativePathToCwd(String(tc()?.title ?? ""), props.cwd())}
            </Text>
            <If when={failed()}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                {ct("status.failed")}
              </Text>
            </If>
            <If when={hasDetails()}>
              <SmeltDisclosureChevron
                open={expanded()}
                size={12}
                color={look().palette.faint}
              />
            </If>
          </View>

          {/* 改动文件差异胶囊 */}
          <View style={{ paddingLeft: 22 }}>
            <DiffFileChips
              files={editFiles}
              cwd={props.cwd}
              machine={props.machine}
              onOpenDiff={props.onOpenDiff}
            />
          </View>

          {/* Pi 子智能体派发卡片 */}
          <If when={(tc()?.subagents?.length ?? 0) > 0}>
            <View style={{ paddingLeft: 22, gap: 4, paddingTop: 2 }}>
              <Each of={tc()?.subagents ?? []}>
                {(runItem) => {
                  const liveMatch = computed(() =>
                    props.liveSubagents().find((s: any) => s.id === runItem().id) ?? null,
                  );
                  const status = computed(() => liveMatch()?.status ?? runItem().status ?? "completed");
                  const isRunning = computed(() => status() === "running" || status() === "pending");
                  return (
                    <View
                      onPress={() =>
                        props.onInspectSubagent({
                          id: runItem().id,
                          title: runItem().title || runItem().id,
                        })
                      }
                      class="hover:bg-accent"
                      style={{
                        paddingHorizontal: 10,
                        paddingVertical: 6,
                        borderRadius: look().radius.md,
                        borderWidth: 1,
                        borderColor: look().palette.hair,
                        backgroundColor: look().palette.surface,
                        flexDirection: "row",
                        alignItems: "center",
                        gap: 8,
                      }}
                    >
                      <If
                        when={isRunning()}
                        else={<SmeltIcon path="icons/bot.svg" size={13} color={look().palette.muted} />}
                      >
                        <SmeltIcon spin size={12} color={look().palette.muted} />
                      </If>
                      <View style={{ flex: 1, minWidth: 0, gap: 1 }}>
                        <Text style={{ fontSize: look().type.small.size, color: look().palette.text, numberOfLines: 1 }}>
                          {runItem().title || runItem().id}
                        </Text>
                        <If when={Boolean(liveMatch()?.lastActivity)}>
                          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint, numberOfLines: 1 }}>
                            {liveMatch()?.lastActivity ?? ""}
                          </Text>
                        </If>
                      </View>
                      <Text
                        style={{
                          fontSize: look().type.text_xsmall.size,
                          color:
                            status() === "failed"
                              ? look().palette.bad
                              : isRunning()
                                ? look().palette.muted
                                : look().palette.faint,
                        }}
                      >
                        {statusLabel(status())}
                      </Text>
                    </View>
                  );
                }}
              </Each>
            </View>
          </If>
        </View>
      </SmeltDisclosure>
    </If>
  );
}

/**
 * 一处改动的增删行数，按 `Diff` 对象缓存：显示行每次重建都会把窗口里的改动过一遍，而按行比对
 * 要把整段文本交给宿主算；合并条目时没变的条目沿用原对象，同一处改动只算一次。
 */
const diffStatsCache = new WeakMap<object, { added: number; removed: number }>();
export function diffPartStats(diff: any): { added: number; removed: number } {
  let stats = diffStatsCache.get(diff);
  if (!stats) {
    stats = diffLineStats(diff.old_text ?? null, diff.new_text ?? "");
    diffStatsCache.set(diff, stats);
  }
  return stats;
}

export function MessageList(props: {
  rows: () => any[];
  machine: () => string | null;
  sessionId: () => string;
  cwd: () => string | null;
  hasOlderHistory: () => boolean;
  loadingOlder: () => boolean;
  atLatest: () => boolean;
  turnActive: () => boolean;
  streamingGlobalIndex: () => number | null;
  streamingText: () => string;
  latestUserGlobalIndex: () => number | null;
  canFork: () => boolean;
  supportsRewind: () => boolean;
  liveSubagents: () => any[];
  onLoadOlder: () => void;
  onViewableRange: (firstRowIdx: number, lastRowIdx: number) => void;
  onScrollAtLatestChange: (atLatest: boolean) => void;
  onToggleTurnFold: (key: string) => void;
  onToggleProcessGroup: (key: string) => void;
  onOpenDiff: (state: EditDiffDialogState) => void;
  onPreviewImage: (img: any) => void;
  onForkAt: (globalIndex: number, role: "user" | "assistant") => void;
  onEditResendAt: (globalIndex: number, text: string) => void;
  onRetryTurn: () => void;
  onInspectSubagent: (sub: { id: string; title: string }) => void;
  onOpenConversation?: (sid: string) => void;
  listRef?: (api: any) => void;
  topPadding?: () => number;
}) {
  const look = theme;
  const expandedTools = signal<Set<string>>(new Set());
  const expandedThoughts = signal<Set<string>>(new Set());
  const expandedTaskNotes = signal<Set<string>>(new Set());
  const copiedKey = signal<string | null>(null);
  let copyResetTimer: ReturnType<typeof setTimeout> | null = null;
  let internalListApi: any = null;

  const copyAnswer = (key: string, text: string) => {
    if (!text) return;
    void clipboardWrite(text).catch(() => {});
    copiedKey.set(key);
    if (copyResetTimer != null) clearTimeout(copyResetTimer);
    copyResetTimer = setTimeout(() => {
      if (copiedKey() === key) copiedKey.set(null);
      copyResetTimer = null;
    }, 1500);
  };

  cleanup(() => {
    if (copyResetTimer != null) {
      clearTimeout(copyResetTimer);
      copyResetTimer = null;
    }
  });

  const toggleSet = (sig: ReturnType<typeof signal<Set<string>>>, key: string) => {
    const next = new Set(sig());
    if (!next.delete(key)) next.add(key);
    sig.set(next);
  };

  const thoughtMarkdownStyles = computed(() => ({
    root: {
      color: look().palette.muted,
      fontSize: look().type.small.size,
      lineHeight: look().type.small.line_height,
    },
  }));

  return (
    <View selectable style={{ position: "relative", flex: 1, width: "100%", minHeight: 0 }}>
      <List
        ref={(api: any) => {
          internalListApi = api;
          props.listRef?.(api);
        }}
        each={props.rows()}
        keyExtractor={(r: any) => r.key}
        initialScrollAtEnd
        alignment="bottom"
        maintainVisibleContentPosition={{ autoscrollToBottomThreshold: 40 }}
        estimatedItemSize={100}
        onLoadMoreAtStart={() => {
          if (props.hasOlderHistory() && !props.loadingOlder()) {
            props.onLoadOlder();
          }
        }}
        viewabilityConfig={{ itemVisibleThreshold: 0.1 }}
        onViewableItemsChange={({ viewableItems }: any) => {
          if (!Array.isArray(viewableItems) || viewableItems.length === 0) return;
          const firstIdx = viewableItems[0]?.index ?? 0;
          const lastIdx = viewableItems[viewableItems.length - 1]?.index ?? firstIdx;
          props.onViewableRange(firstIdx, lastIdx);
          const total = props.rows().length;
          const nearEnd = total === 0 || lastIdx >= total - 2;
          props.onScrollAtLatestChange(nearEnd);
        }}
        style={{ flex: 1, width: "100%", minHeight: 0 }}
        contentStyle={{
          paddingTop: props.topPadding ? props.topPadding() : 16,
          paddingBottom: 20,
          paddingHorizontal: 24,
          gap: 12,
        }}
        header={
          <If when={props.hasOlderHistory()}>
            <View style={{ width: "100%", alignItems: "center", paddingBottom: 8 }}>
              <SmeltButton
                quiet
                small
                loading={props.loadingOlder()}
                label={ct("status.load_older")}
                onPress={props.onLoadOlder}
              />
            </View>
          </If>
        }
      >
        {(row: () => any) => {
          const r = () => row();
          const kind = () => r().kind;
          const entry = () => r().entry;

          return (
            <View style={{ width: "100%", maxWidth: 768, alignSelf: "center" }}>
              {/* 1. 折叠的整轮思考/工具汇总行 */}
              <If when={kind() === "turn_fold"}>
                <View
                  onPress={() => props.onToggleTurnFold(r().key)}
                  class="hover:bg-accent"
                  style={{
                    height: 28,
                    paddingHorizontal: 8,
                    borderRadius: look().radius.row,
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 6,
                  }}
                >
                  <SmeltDisclosureChevron
                    open={r().expanded}
                    size={12}
                    color={look().palette.faint}
                  />
                  <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                    {summarizeTally(r().fold?.tally)}
                  </Text>
                  <If when={r().fold?.elapsedMs != null}>
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                      · {formatDuration(r().fold?.elapsedMs)}
                    </Text>
                  </If>
                </View>
              </If>

              {/* 2. 折叠的工具过程分组行 + 改动文件差异标签 */}
              <If when={kind() === "process_group"}>
                <View style={{ width: "100%", gap: 4 }}>
                  <View
                    onPress={() => props.onToggleProcessGroup(r().key)}
                    class="hover:bg-accent"
                    style={{
                      height: 28,
                      paddingHorizontal: 8,
                      borderRadius: look().radius.row,
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 6,
                    }}
                  >
                    <If
                      when={Boolean(r().group?.active && (r().group?.tally?.running ?? 0) > 0)}
                      else={
                        <SmeltDisclosureChevron
                          open={r().expanded}
                          size={12}
                          color={look().palette.faint}
                        />
                      }
                    >
                      <SmeltIcon spin size={12} color={look().palette.muted} />
                    </If>
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                      {summarizeTally(r().group?.tally)}
                    </Text>
                    <If when={r().group?.showElapsed && r().group?.elapsedMs != null}>
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                        · {formatDuration(r().group?.elapsedMs)}
                      </Text>
                    </If>
                  </View>
                  <If when={!r().expanded && (r().editFiles?.length ?? 0) > 0}>
                    <View style={{ paddingLeft: 18 }}>
                      <DiffFileChips
                        files={() => r().editFiles ?? []}
                        cwd={props.cwd}
                        machine={props.machine}
                        onOpenDiff={props.onOpenDiff}
                      />
                    </View>
                  </If>
                </View>
              </If>

              {/* 3. 具体消息条目 */}
              <If when={kind() === "entry"}>
                <View style={{ width: "100%", paddingLeft: r().inProcess ? 14 : 0 }}>
                  {/* 3a. User / UserWithImages */}
                  <If when={isUserEntry(entry())}>
                    {(() => {
                      const userText = () => entryUserText(entry());
                      const userImages = () =>
                        entry()?.UserWithImages?.images ?? [];
                      const interrupted = () => isInterruptMarker(userText());
                      const isLatestUser = () =>
                        props.latestUserGlobalIndex() === r().globalIndex && !props.turnActive();
                      return (
                        <If
                          when={!interrupted()}
                          else={
                            <View style={{ paddingVertical: 4 }}>
                              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                                {userText()}
                              </Text>
                            </View>
                          }
                        >
                          <View style={{ width: "100%", alignItems: "flex-end", paddingTop: 6 }}>
                            <MessageContextWrapper
                              machine={props.machine}
                              sessionId={props.sessionId}
                              role="user"
                              globalIndex={() => r().globalIndex}
                              copyText={userText}
                              canFork={() =>
                                props.canFork() &&
                                !props.turnActive() &&
                                Boolean(props.supportsRewind()) &&
                                !isLatestUser()
                              }
                              canEditResend={() => Boolean(props.supportsRewind() && isLatestUser())}
                              onFork={() => props.onForkAt(r().globalIndex, "user")}
                              onEditResend={() => props.onEditResendAt(r().globalIndex, userText())}
                            >
                              <View
                                class="rounded-[20px] bg-foreground/[0.08] px-4 py-2.5 gap-2"
                                style={{ maxWidth: "72%" }}
                              >
                                <If when={userImages().length > 0}>
                                  <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
                                    <Each of={userImages()}>
                                      {(img) => {
                                        const src = computed(() => imageThumbSrc(img()));
                                        return (
                                          <If when={src() != null}>
                                            <View
                                              onPress={() => props.onPreviewImage(img())}
                                              class="rounded-[10px] overflow-hidden border border-border"
                                            >
                                              <Image
                                                src={src()!}
                                                style={{ width: 104, height: 78, objectFit: "cover" }}
                                              />
                                            </View>
                                          </If>
                                        );
                                      }}
                                    </Each>
                                  </View>
                                </If>
                                <If when={userText().trim().length > 0}>
                                  <Text
                                    class="text-foreground"
                                    style={{
                                      fontSize: (14 / 16) * (look().rem ?? 16),
                                      lineHeight: (21 / 16) * (look().rem ?? 16),
                                    }}
                                  >
                                    {userText()}
                                  </Text>
                                </If>
                              </View>
                            </MessageContextWrapper>
                          </View>
                        </If>
                      );
                    })()}
                  </If>

                  {/* 3b. Assistant (thought 或 正文) */}
                  <If when={Boolean(entry()?.Assistant)}>
                    {(() => {
                      const isStreamingCell = () =>
                        props.turnActive() && props.streamingGlobalIndex() === r().globalIndex;
                      const bodyText = () =>
                        isStreamingCell()
                          ? props.streamingText()
                          : String(entry()?.Assistant?.text ?? "");
                      const isThought = () => Boolean(entry()?.Assistant?.thought);
                      const thoughtExpanded = () =>
                        expandedThoughts().has(r().key) || isStreamingCell();
                      const showActionSlot = () =>
                        Boolean(r().presentation?.finalAnswer) || isStreamingCell();
                      const showActions = () =>
                        Boolean(r().presentation?.finalAnswer) && !isStreamingCell();
                      return (
                        <If
                          when={!isThought()}
                          else={
                            <SmeltDisclosure
                              open={thoughtExpanded()}
                              style={{ width: "100%" }}
                              content={
                                <View
                                  style={{
                                    marginTop: 4,
                                    paddingLeft: 18,
                                    borderLeftWidth: 2,
                                    borderLeftColor: look().palette.hair,
                                  }}
                                >
                                  <SmeltMarkdown
                                    markdown={bodyText()}
                                    streaming={isStreamingCell()}
                                    cwd={props.cwd()}
                                    machine={props.machine()}
                                    styles={thoughtMarkdownStyles()}
                                  />
                                </View>
                              }
                            >
                              <View
                                onPress={() => toggleSet(expandedThoughts, r().key)}
                                class="hover:bg-accent"
                                style={{
                                  height: 26,
                                  paddingHorizontal: 8,
                                  borderRadius: look().radius.row,
                                  flexDirection: "row",
                                  alignItems: "center",
                                  gap: 6,
                                }}
                              >
                                <SmeltDisclosureChevron
                                  open={thoughtExpanded()}
                                  size={12}
                                  color={look().palette.faint}
                                />
                                <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                                  {isStreamingCell() ? ct("status.thinking") : ct("status.thinking").replace("…", "")}
                                </Text>
                              </View>
                            </SmeltDisclosure>
                          }
                        >
                          <MessageContextWrapper
                            machine={props.machine}
                            sessionId={props.sessionId}
                            role="assistant"
                            globalIndex={() => r().globalIndex}
                            copyText={bodyText}
                            canFork={() => props.canFork() && !props.turnActive()}
                            canEditResend={() => false}
                            onFork={() => props.onForkAt(r().globalIndex, "assistant")}
                            onEditResend={() => {}}
                          >
                            <View style={{ width: "100%", paddingVertical: 4, gap: 4 }}>
                              <SmeltMarkdown
                                markdown={bodyText()}
                                streaming={isStreamingCell()}
                                cwd={props.cwd()}
                                machine={props.machine()}
                              />
                              <If when={showActionSlot()}>
                                <View
                                  style={{
                                    height: 24,
                                    paddingTop: 2,
                                    flexDirection: "row",
                                    alignItems: "center",
                                    gap: 2,
                                    opacity: showActions() ? 1 : 0,
                                    pointerEvents: showActions() ? "auto" : "none",
                                    transitionProperty: "opacity",
                                    transitionDuration: 180,
                                  }}
                                >
                                  <If when={showActions()}>
                                    <SmeltButton
                                      quiet
                                      small
                                      icon={copiedKey() === r().key ? "icons/check.svg" : "icons/copy.svg"}
                                      tooltip={copiedKey() === r().key ? ct("action.copied") : ct("action.copy")}
                                      onPress={() => copyAnswer(r().key, bodyText())}
                                    />
                                    <If when={props.canFork() && !props.turnActive()}>
                                      <SmeltButton
                                        quiet
                                        small
                                        icon="icons/git-branch.svg"
                                        tooltip={ct("action.fork")}
                                        onPress={() => props.onForkAt(r().globalIndex, "assistant")}
                                      />
                                    </If>
                                  </If>
                                </View>
                              </If>
                            </View>
                          </MessageContextWrapper>
                        </If>
                      );
                    })()}
                  </If>

                  {/* 3c. ToolCall */}
                  <If when={Boolean(entry()?.ToolCall)}>
                    <ToolCallRow
                      tc={() => entry()?.ToolCall}
                      cwd={props.cwd}
                      machine={props.machine}
                      liveSubagents={props.liveSubagents}
                      expandedTools={expandedTools}
                      onToggleTool={(id) => toggleSet(expandedTools, id)}
                      onOpenDiff={props.onOpenDiff}
                      onPreviewImage={props.onPreviewImage}
                      onInspectSubagent={props.onInspectSubagent}
                      onOpenConversation={props.onOpenConversation}
                    />
                  </If>

                  {/* 3d. Divider / NewSession */}
                  <If when={typeof entry()?.Divider === "string" || Boolean(entry()?.NewSession)}>
                    {(() => {
                      const label = () =>
                        typeof entry()?.Divider === "string"
                          ? entry().Divider
                          : ct("entry.new_session", {
                              at: formatNewSessionTime(entry()?.NewSession?.at_ms ?? Date.now()),
                            });
                      return (
                        <View
                          style={{
                            flexDirection: "row",
                            alignItems: "center",
                            gap: 10,
                            paddingVertical: 10,
                          }}
                        >
                          <View style={{ flex: 1, height: 1, backgroundColor: look().palette.hair }} />
                          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                            {label()}
                          </Text>
                          <View style={{ flex: 1, height: 1, backgroundColor: look().palette.hair }} />
                        </View>
                      );
                    })()}
                  </If>

                  {/* 3e. TurnFailed */}
                  <If when={Boolean(entry()?.TurnFailed)}>
                    {(() => {
                      const tf = () => entry()?.TurnFailed;
                      const hint = () => turnHintMessage(tf()?.hint);
                      return (
                        <View class="px-3.5 py-2.5 rounded-xl border border-bad bg-bad/12 gap-1.5">
                          <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
                            <SmeltIcon path="icons/circle-x.svg" size={14} color={look().palette.bad} />
                            <Text
                              style={{
                                flex: 1,
                                fontSize: look().type.small_medium.size,
                                fontWeight: look().type.small_medium.weight,
                                color: look().palette.bad,
                              }}
                            >
                              {ct("entry.turn_failed")}
                            </Text>
                            <If
                              when={
                                props.supportsRewind() &&
                                !props.turnActive() &&
                                r().globalIndex > (props.latestUserGlobalIndex() ?? -1)
                              }
                            >
                              <SmeltButton
                                quiet
                                small
                                label={ct("action.retry_turn")}
                                onPress={props.onRetryTurn}
                              />
                            </If>
                          </View>
                          <If when={Boolean(tf()?.detail)}>
                            <Text style={{ fontSize: look().type.small.size, color: look().palette.text }}>
                              {tf()?.detail ?? ""}
                            </Text>
                          </If>
                          <If when={Boolean(hint())}>
                            <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                              {hint()}
                            </Text>
                          </If>
                        </View>
                      );
                    })()}
                  </If>

                  {/* 3f. TaskNote */}
                  <If when={Boolean(entry()?.TaskNote)}>
                    {(() => {
                      const note = () => entry()?.TaskNote;
                      const noteKey = () => r().key;
                      const expanded = () => expandedTaskNotes().has(noteKey());
                      const tail = () => String(note()?.outputTail ?? note()?.output_tail ?? "").trim();
                      const headline = () => {
                        const n = note();
                        if (n?.peer) {
                          const from = n.peer.sourceTitle || n.peer.sourceSessionId || "";
                          return ct("peer.received", { from });
                        }
                        const title = n?.title || n?.id || "";
                        const st = statusLabel(n?.status ?? "");
                        return title ? `${title} · ${st}` : st;
                      };
                      return (
                        <SmeltDisclosure
                          open={expanded() && tail().length > 0}
                          style={{
                            paddingHorizontal: 10,
                            paddingVertical: 6,
                            borderRadius: look().radius.md,
                            borderWidth: 1,
                            borderColor: look().palette.hair,
                            backgroundColor: look().palette.surface,
                          }}
                          content={
                            <Scroll style={{ maxHeight: 140, marginTop: 4, paddingLeft: 18 }}>
                              <Text
                                style={{
                                  fontFamily: look().fonts?.mono,
                                  fontSize: look().type.text_xsmall.size,
                                  color: look().palette.muted,
                                }}
                              >
                                {tail()}
                              </Text>
                            </Scroll>
                          }
                        >
                          <View
                            onPress={() => {
                              if (tail()) toggleSet(expandedTaskNotes, noteKey());
                            }}
                            style={{ flexDirection: "row", alignItems: "center", gap: 6 }}
                          >
                            <SmeltIcon path="icons/info.svg" size={12} color={look().palette.muted} />
                            <Text style={{ flex: 1, fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                              {headline()}
                            </Text>
                            <If when={tail().length > 0}>
                              <SmeltDisclosureChevron
                                open={expanded()}
                                size={12}
                                color={look().palette.faint}
                              />
                            </If>
                          </View>
                        </SmeltDisclosure>
                      );
                    })()}
                  </If>
                </View>
              </If>
            </View>
          );
        }}
      </List>

      {/* 离底时居中浮现「回到最新」浮起胶囊 */}
      <If when={!props.atLatest() && props.rows().length > 0}>
        <View
          style={{
            position: "absolute",
            left: 0,
            right: 0,
            bottom: 14,
            alignItems: "center",
            pointerEvents: "box-none",
          }}
        >
          <SmeltButton
            floating
            small
            icon="icons/arrow-down.svg"
            label={ct("action.jump_latest")}
            onPress={() => {
              internalListApi?.scrollToEnd?.({ animated: true });
              props.onScrollAtLatestChange(true);
            }}
          />
        </View>
      </If>
    </View>
  );
}
