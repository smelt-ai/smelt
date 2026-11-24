// Pi 子智能体与后台任务入口：工具栏计数胶囊、弹层列表、以及查看单个子智能体过程的侧边抽屉。
import { cleanup, computed, effect, Scroll, signal, Text, View } from "nsh";
import { Popover } from "nsh/base";
import { call, SmeltButton, SmeltIcon, SmeltMarkdown, theme } from "../sdk.tsx";
import { ct } from "./i18n.ts";
import { formatDuration, runningBackgroundTasks, runningSubagents } from "./model.js";

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
      <Popover
        open={open()}
        onOpenChange={({ open: next }: any) => open.set(Boolean(next))}
        placement="top"
        align="start"
        offset={8}
        popupClass="rounded-xl border border-border bg-popover p-3 shadow-lg"
        popupStyle={{ width: 344 }}
        trigger={
          <SmeltButton
            quiet
            small
            selected={open()}
            icon="icons/bot.svg"
            label={ct("subagent.summary", { count: active().length })}
            style={{ borderRadius: 999 }}
          />
        }
      >
        <View style={{ gap: 8 }}>
          <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between" }}>
            <Text style={{ fontSize: look().type.small_medium?.size ?? 12, color: look().palette.muted }}>
              {ct("subagent.title")}
            </Text>
            <SmeltButton
              quiet
              danger
              small
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
                  <View
                    onPress={() => {
                      open.set(false);
                      props.onInspect(sub());
                    }}
                    class="hover:bg-accent"
                    style={{
                      paddingHorizontal: 8,
                      paddingVertical: 6,
                      borderRadius: 6,
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 8,
                    }}
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
                      quiet
                      danger
                      small
                      label={ct("action.stop")}
                      onPress={() => props.onStop(sub().id)}
                    />
                  </View>
                )}
              </Each>
            </View>
          </Scroll>
        </View>
      </Popover>
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
      <Popover
        open={open()}
        onOpenChange={({ open: next }: any) => open.set(Boolean(next))}
        placement="top"
        align="start"
        offset={8}
        popupClass="rounded-xl border border-border bg-popover p-3 shadow-lg"
        popupStyle={{ width: 344 }}
        trigger={
          <SmeltButton
            quiet
            small
            selected={open()}
            icon="icons/terminal.svg"
            label={ct("task.summary", { count: active().length })}
            style={{ borderRadius: 999 }}
          />
        }
      >
        <View style={{ gap: 8 }}>
          <Text style={{ fontSize: look().type.small_medium?.size ?? 12, color: look().palette.muted }}>
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
                      borderRadius: 6,
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
                      quiet
                      danger
                      small
                      label={ct("action.stop")}
                      onPress={() => props.onStop(task().id)}
                    />
                  </View>
                )}
              </Each>
            </View>
          </Scroll>
        </View>
      </Popover>
    </If>
  );
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
  const remoteEntries = signal<any[]>([]);
  const remoteOffset = signal(0);
  const remoteLoading = signal(false);

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

  const liveRevision = computed(() => {
    const target = props.inspected();
    if (!target) return -1;
    const match = (props.subagents?.() ?? []).find((s: any) => s?.id === target.id);
    return typeof match?.revision === "number" ? match.revision : 0;
  });

  // Pi 子智能体有独立子会话文件，不挂在父 ToolCall.children，需通过 `subagent.transcript` RPC 拉取。
  effect(() => {
    const target = props.inspected();
    const rev = liveRevision();
    const machine = props.machine();
    void rev;
    if (!target) {
      remoteEntries.set([]);
      remoteOffset.set(0);
      remoteLoading.set(false);
      return;
    }
    if (inlineEntries().length > 0) return;
    let cancelled = false;
    remoteLoading.set(true);
    void call(machine, "subagent.transcript", {
      subagent_id: target.id,
      limit: 200,
    })
      .then((res: any) => {
        if (cancelled) return;
        remoteEntries.set(Array.isArray(res?.entries) ? res.entries : []);
        remoteOffset.set(typeof res?.offset === "number" ? res.offset : 0);
      })
      .catch(() => {
        if (!cancelled) remoteEntries.set([]);
      })
      .finally(() => {
        if (!cancelled) remoteLoading.set(false);
      });
    cleanup(() => {
      cancelled = true;
    });
  });

  const nestedEntries = computed(() => {
    const inline = inlineEntries();
    return inline.length > 0 ? inline : remoteEntries();
  });

  const loadOlderRemote = () => {
    const target = props.inspected();
    const before = remoteOffset();
    if (!target || before <= 0 || remoteLoading()) return;
    remoteLoading.set(true);
    void call(props.machine(), "subagent.transcript", {
      subagent_id: target.id,
      before,
      limit: 200,
    })
      .then((res: any) => {
        const older = Array.isArray(res?.entries) ? res.entries : [];
        if (older.length > 0) {
          remoteEntries.set([...older, ...remoteEntries()]);
        }
        remoteOffset.set(typeof res?.offset === "number" ? res.offset : 0);
      })
      .catch(() => {})
      .finally(() => remoteLoading.set(false));
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
              fontSize: look().type.small_medium?.size ?? 12,
              color: look().palette.text,
              numberOfLines: 1,
            }}
          >
            {props.inspected()?.title ?? ct("subagent.title")}
          </Text>
          <SmeltButton quiet small icon="icons/close.svg" onPress={props.onClose} />
        </View>
        <Scroll style={{ flex: 1, minHeight: 0 }}>
          <View style={{ padding: 12, gap: 10 }}>
            <If when={inlineEntries().length === 0 && remoteOffset() > 0}>
              <SmeltButton
                quiet
                small
                loading={remoteLoading()}
                label={ct("status.load_older")}
                onPress={loadOlderRemote}
              />
            </If>
            <Each of={nestedEntries()}>
              {(child) => (
                <View style={{ gap: 4 }}>
                  <If when={Boolean(child()?.Assistant)}>
                    <SmeltMarkdown
                      markdown={child().Assistant.text ?? ""}
                      cwd={props.cwd()}
                      machine={props.machine()}
                      styles={
                        child().Assistant.thought
                          ? { root: { color: look().palette.muted, fontSize: look().type.text_xsmall.size } }
                          : undefined
                      }
                    />
                  </If>
                  <If when={Boolean(child()?.ToolCall)}>
                    <View
                      style={{
                        paddingHorizontal: 8,
                        paddingVertical: 5,
                        borderRadius: 6,
                        backgroundColor: look().palette.bg,
                        borderWidth: 1,
                        borderColor: look().palette.hair,
                      }}
                    >
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                        {child().ToolCall.title}
                      </Text>
                    </View>
                  </If>
                </View>
              )}
            </Each>
          </View>
        </Scroll>
      </View>
    </If>
  );
}
