// Pi 子智能体与后台任务入口：工具栏计数胶囊、弹层列表、以及查看单个子智能体过程的侧边抽屉。
import { cleanup, computed, effect, List, Scroll, signal, Text, untracked, View } from "nsh";
import {
  blobUrl,
  callWithBlobs,
  mergeEntryWindow,
  SmeltButton,
  SmeltIcon,
  SmeltMarkdown,
  SmeltPopover,
  SmeltRow,
  theme,
} from "smelt/sdk";
import type { BlobAt, EntryWindow } from "smelt/sdk";
import { ct } from "./i18n.ts";
import {
  EMPTY_ENTRY_WINDOW,
  formatDuration,
  runningBackgroundTasks,
  runningSubagents,
  spliceEntries,
  transcriptWindowOf,
} from "./model.js";

export function SubagentsControl(props: {
  subagents: () => any[];
  onStop: (id: string) => void;
  onInspect: (subagent: any) => void;
}) {
  const look = theme;
  const open = signal(false);
  const active = computed(() => runningSubagents(props.subagents()));

  return (
    <If when={active().length > 0}>
      <SmeltPopover
        open={open()}
        onOpenChange={({ open: next }: any) => open.set(Boolean(next))}
        placement="top"
        align="start"
        offset={8}
        popupClass="rounded-xl border border-border bg-popover p-3 shadow-md"
        popupStyle={{ width: 344 }}
        trigger={
          <SmeltButton
            variant="quiet"
            size="sm"
            selected={open()}
            icon="icons/bot.svg"
            label={ct("subagent.summary", { count: active().length })}
            shape="pill"
          />
        }
      >
        <View style={{ gap: 8 }}>
          <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between" }}>
            <Text style={{ fontSize: look().type.small_medium.size, color: look().palette.muted }}>
              {ct("subagent.title")}
            </Text>
            <SmeltButton
              variant="danger-quiet"
              size="sm"
              label={ct("subagent.stop_all")}
              onPress={() => {
                for (const s of active()) props.onStop(s.id);
              }}
            />
          </View>
          <Scroll style={{ maxHeight: 280 }}>
            <View style={{ gap: 4 }}>
              <Each of={active()}>
                {(sub) => (
                  <SmeltRow
                    onPress={() => {
                      open.set(false);
                      props.onInspect(sub());
                    }}
                    style={{ paddingHorizontal: 8, paddingVertical: 6, gap: 8 }}
                  >
                    <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
                      <Text style={{ fontSize: look().type.small.size, color: look().palette.text, numberOfLines: 1 }}>
                        {sub().title}
                      </Text>
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint, numberOfLines: 1 }}>
                        {sub().lastActivity || sub().last_activity || ct("subagent.preparing")}
                      </Text>
                    </View>
                    <SmeltButton
                      variant="danger-quiet"
                      size="sm"
                      label={ct("action.stop")}
                      onPress={() => props.onStop(sub().id)}
                    />
                  </SmeltRow>
                )}
              </Each>
            </View>
          </Scroll>
        </View>
      </SmeltPopover>
    </If>
  );
}

export function BackgroundTasksControl(props: {
  tasks: () => any[];
  onStop: (id: string) => void;
}) {
  const look = theme;
  const open = signal(false);
  const active = computed(() => runningBackgroundTasks(props.tasks()));

  return (
    <If when={active().length > 0}>
      <SmeltPopover
        open={open()}
        onOpenChange={({ open: next }: any) => open.set(Boolean(next))}
        placement="top"
        align="start"
        offset={8}
        popupClass="rounded-xl border border-border bg-popover p-3 shadow-md"
        popupStyle={{ width: 344 }}
        trigger={
          <SmeltButton
            variant="quiet"
            size="sm"
            selected={open()}
            icon="icons/terminal.svg"
            label={ct("task.summary", { count: active().length })}
            shape="pill"
          />
        }
      >
        <View style={{ gap: 8 }}>
          <Text style={{ fontSize: look().type.small_medium.size, color: look().palette.muted }}>
            {ct("task.title")}
          </Text>
          <Scroll style={{ maxHeight: 240 }}>
            <View style={{ gap: 4 }}>
              <Each of={active()}>
                {(task) => (
                  <View
                    style={{
                      paddingHorizontal: 8,
                      paddingVertical: 6,
                      borderRadius: look().radius.md,
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 8,
                    }}
                  >
                    <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
                      <Text
                        style={{
                          fontFamily: look().fonts?.mono,
                          fontSize: look().type.text_xsmall.size,
                          color: look().palette.text,
                          numberOfLines: 1,
                        }}
                      >
                        {task().command || task().id}
                      </Text>
                      <If when={(task().startedAtMs ?? task().started_at_ms) != null}>
                        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                          {formatDuration(Math.max(0, Date.now() - (task().startedAtMs ?? task().started_at_ms)))}
                        </Text>
                      </If>
                    </View>
                    <SmeltButton
                      variant="danger-quiet"
                      size="sm"
                      label={ct("action.stop")}
                      onPress={() => props.onStop(task().id)}
                    />
                  </View>
                )}
              </Each>
            </View>
          </Scroll>
        </View>
      </SmeltPopover>
    </If>
  );
}

/** 一页几条。 */
const TRANSCRIPT_PAGE = 200;
/** 子会话里的回答以 Blob 交来，只转交给 Markdown，不进插件的 JS 堆。 */
const TRANSCRIPT_BLOBS: BlobAt[] = [{ at: "/entries", each: "/Assistant/text" }];

/** 手里连续的一段子会话。`cursor` 是跟着跑那条线上次读到哪儿；老运行时不给，每次取最后一页。 */
interface Transcript {
  entries: any[];
  window: EntryWindow;
  cursor: number | null;
}

const EMPTY_TRANSCRIPT: Transcript = { entries: [], window: EMPTY_ENTRY_WINDOW, cursor: null };

/** 把取回的一页按窗口规则（和对话同一份）拼进手里的。只有跟着跑的那条线推进 `cursor`。 */
function mergeTranscript(current: Transcript, page: any, follows: boolean): Transcript {
  const plan = mergeEntryWindow(current.window, transcriptWindowOf(page));
  return {
    entries: spliceEntries(current.entries, plan, Array.isArray(page?.entries) ? page.entries : []),
    window: plan.window,
    cursor: follows ? (typeof page?.cursor === "number" ? page.cursor : null) : current.cursor,
  };
}

/** 正在看的远端子会话：同一时间只有一个跟着跑的请求在路上，期间又变了就等它回来再取一次。 */
interface Follow {
  id: string;
  machine: string | null;
  busy: boolean;
  again: boolean;
}

export function SubagentDrawer(props: {
  inspected: () => { id: string; title: string } | null;
  entries: () => any[];
  subagents?: () => any[];
  cwd: () => string | null;
  machine: () => string | null;
  onClose: () => void;
}) {
  const look = theme;
  const transcript = signal<Transcript>(EMPTY_TRANSCRIPT);
  const loadingOlder = signal(false);

  const inlineEntries = computed(() => {
    const target = props.inspected();
    if (!target) return [];
    for (const e of props.entries()) {
      const tc = e?.ToolCall;
      if (!tc) continue;
      if (tc.id === target.id && Array.isArray(tc.children)) return tc.children;
      if (Array.isArray(tc.subagents) && tc.subagents.some((s: any) => s.id === target.id)) {
        return tc.children ?? [];
      }
    }
    return [];
  });
  // 只认有没有：`inlineEntries` 随父对话每一帧重算，跟着跑别因此重取。
  const hasInline = computed(() => inlineEntries().length > 0);

  const liveRevision = computed(() => {
    const target = props.inspected();
    if (!target) return -1;
    const match = (props.subagents?.() ?? []).find((s: any) => s?.id === target.id);
    return typeof match?.revision === "number" ? match.revision : 0;
  });

  let follow: Follow | null = null;
  cleanup(() => {
    follow = null;
  });

  const fetchTranscript = (f: Follow, params: Record<string, unknown>) =>
    callWithBlobs(
      f.machine,
      "subagent.transcript",
      { subagent_id: f.id, limit: TRANSCRIPT_PAGE, ...params },
      TRANSCRIPT_BLOBS,
    );

  // 跟着跑：打开时取最后一页，之后子会话每多一条消息带上次的 `cursor`，只取变了的。
  const refresh = (f: Follow) => {
    if (follow !== f) return;
    if (f.busy) {
      f.again = true;
      return;
    }
    f.busy = true;
    const since = untracked(() => transcript().cursor);
    void fetchTranscript(f, since == null ? {} : { since })
      .then(
        (page: any) => {
          if (follow === f) transcript.set(mergeTranscript(untracked(transcript), page, true));
        },
        (error: unknown) => {
          if (follow === f) console.error("[subagent] 读取子会话失败：", error);
        },
      )
      .finally(() => {
        f.busy = false;
        if (follow === f && f.again) {
          f.again = false;
          refresh(f);
        }
      });
  };

  // Pi 子智能体有独立子会话文件，不挂在父 ToolCall.children，经 `subagent.transcript` 取。
  effect(() => {
    const target = props.inspected();
    const machine = props.machine();
    liveRevision();
    if (!target || hasInline()) {
      follow = null;
      transcript.set(EMPTY_TRANSCRIPT);
      loadingOlder.set(false);
      return;
    }
    if (follow?.id !== target.id || follow.machine !== machine) {
      follow = { id: target.id, machine, busy: false, again: false };
      transcript.set(EMPTY_TRANSCRIPT);
      loadingOlder.set(false);
    }
    refresh(follow);
  });

  const nestedEntries = computed(() => (hasInline() ? inlineEntries() : transcript().entries));
  const hasOlder = computed(() => !hasInline() && transcript().window.offset > 0);

  const loadOlder = () => {
    const f = follow;
    const before = transcript().window.offset;
    if (!f || before <= 0 || loadingOlder()) return;
    loadingOlder.set(true);
    void fetchTranscript(f, { before })
      .then(
        (page: any) => {
          if (follow === f) transcript.set(mergeTranscript(transcript(), page, false));
        },
        (error: unknown) => {
          if (follow === f) console.error("[subagent] 读取更早的子会话失败：", error);
        },
      )
      .finally(() => {
        if (follow === f) loadingOlder.set(false);
      });
  };

  return (
    <If when={props.inspected() != null}>
      <View
        style={{
          width: 380,
          height: "100%",
          borderLeftWidth: 1,
          borderLeftColor: look().palette.hair,
          backgroundColor: look().palette.surface,
          display: "flex",
          flexDirection: "column",
          flexShrink: 0,
        }}
      >
        <View
          style={{
            height: 40,
            paddingHorizontal: 12,
            flexDirection: "row",
            alignItems: "center",
            gap: 8,
            borderBottomWidth: 1,
            borderBottomColor: look().palette.hair,
          }}
        >
          <SmeltIcon path="icons/bot.svg" size={14} color={look().palette.muted} />
          <Text
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: look().type.small_medium.size,
              color: look().palette.text,
              numberOfLines: 1,
            }}
          >
            {props.inspected()?.title ?? ct("subagent.title")}
          </Text>
          <SmeltButton variant="quiet" size="sm" icon="icons/close.svg" onPress={props.onClose} />
        </View>
        <List
          each={nestedEntries()}
          keyExtractor={(_entry: any, ix: number) =>
            `${props.inspected()?.id ?? ""}:${(hasInline() ? 0 : transcript().window.offset) + ix}`
          }
          initialScrollAtEnd
          alignment="bottom"
          maintainVisibleContentPosition={{ autoscrollToBottomThreshold: 96 }}
          estimatedItemSize={60}
          onLoadMoreAtStart={loadOlder}
          style={{ flex: 1, minHeight: 0 }}
          contentStyle={{ padding: 12, gap: 10 }}
          header={
            <If when={hasOlder()}>
              <SmeltButton
                variant="quiet"
                size="sm"
                loading={loadingOlder()}
                label={ct("status.load_older")}
                onPress={loadOlder}
              />
            </If>
          }
        >
          {(entry: () => any) => (
            <SubagentEntry entry={entry()} cwd={props.cwd()} machine={props.machine()} />
          )}
        </List>
      </View>
    </If>
  );
}

/** 子过程里的一条：回答交给 Markdown，工具只写标题。远端子会话的回答是 Blob，内嵌的是正文。 */
function SubagentEntry(props: { entry: any; cwd: string | null; machine: string | null }) {
  const look = theme;
  const answer = () => props.entry?.Assistant;
  const markdownUrl = blobUrl(() => (answer()?.text instanceof Blob ? answer().text : null));
  return (
    <View style={{ gap: 4 }}>
      <If when={answer() != null}>
        <SmeltMarkdown
          markdown={typeof answer()?.text === "string" ? answer().text : undefined}
          markdownUrl={markdownUrl()}
          cwd={props.cwd}
          machine={props.machine}
          styles={
            answer()?.thought
              ? { root: { color: look().palette.muted, fontSize: look().type.text_xsmall.size } }
              : undefined
          }
        />
      </If>
      <If when={props.entry?.ToolCall != null}>
        <View
          style={{
            paddingHorizontal: 8,
            paddingVertical: 5,
            borderRadius: look().radius.md,
            backgroundColor: look().palette.bg,
            borderWidth: 1,
            borderColor: look().palette.hair,
          }}
        >
          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
            {props.entry.ToolCall.title}
          </Text>
        </View>
      </If>
    </View>
  );
}
