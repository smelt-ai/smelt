// 对话底部输入栏：权限审批卡 / Elicitation 表单 / 计划栏 / 排队提示 / 附件缩略图 / `@`/`/` 补全弹层 /
// 多行自适应输入框（Enter 发送、Shift+Enter 换行、零拷贝截图粘贴、+ 选文件）/ 模型、能力、子智能体、后台任务与用量工具栏。
import {
  computed,
  effect,
  Image,
  keymap,
  reduceMotion,
  Scroll,
  signal,
  Text,
  View,
} from "nsh";
import { Input } from "nsh/base";
import {
  acpClipboardImages,
  acpPasteImage,
  call,
  fileUriToPath,
  localOs,
  run,
  SmeltButton,
  SmeltDisclosure,
  SmeltDisclosureChevron,
  SmeltIcon,
  SmeltRow,
  SmeltVector,
  theme,
  windowActive,
} from "smelt/sdk";
import { ElicitationCard, PermissionTray } from "./cards.tsx";
import { ct } from "./i18n.ts";
import {
  buildCompletionCandidates,
  colorWithAlpha,
  detectCompletionTrigger,
  isNewConversationCommand,
  STOP_RING_INSET,
  STOP_RING_SIZE,
  stopRingMarkup,
  stopRingMode,
} from "./model.js";
import { CapabilitiesPopover, ModelConfigPopover, UsagePopover } from "./model_popover.tsx";
import { BackgroundTasksControl, SubagentsControl } from "./subagents.tsx";

export interface AttachedImage {
  ref_id: string;
  mime: string;
  preview_uri: string;
  width: number;
  height: number;
}

/** 输入框的命令式接口：外面要往草稿里放东西时直接调，不经信号接力。 */
export interface ComposerApi {
  /** 接在草稿后面（草稿不是空格或换行结尾就先补一个空格），光标放到末尾并聚焦。 */
  insert(text: string): void;
  /** 把没发出去的草稿放回来：文字放在现有草稿前面，图片接在已有附件后面，并聚焦。 */
  restore(text: string, images: AttachedImage[]): void;
}

const IMAGE_EXT_RE = /\.(png|jpe?g|gif|webp)$/i;
/** 空对话时输入区最低留几行。 */
const ROOMY_INPUT_LINES = 3;

export function Composer(props: {
  machine: () => string | null;
  cwd: () => string | null;
  visible?: () => boolean;
  readOnly: () => boolean;
  turnActive: () => boolean;
  dead: () => boolean;
  supportsImage: () => boolean;
  supportsCompaction: () => boolean;
  compacting: () => boolean;
  fallbackModelLabel: () => string;
  model: () => any | null;
  configs: () => any[];
  pendingConfigCount: () => number;
  availableCommands: () => any[];
  skills: () => { command: string; name: string; description: string }[];
  mcpServers: () => string[];
  capabilitiesByAgent: () => boolean;
  permissions: () => any[];
  permissionSubmitting: () => boolean;
  elicitation: () => any | null;
  plan: () => any | null;
  pendingPrompts: () => any[];
  queuedSteering: () => string[];
  queuedFollowUp: () => string[];
  subagents: () => any[];
  backgroundTasks: () => any[];
  usage: () => [number, number] | null;
  usageBreakdown: () => any | null;
  focusSeq: () => number;
  /** 空对话：输入框在页面中间当主角，最低留三行；进了对话回到两行，不挤消息。 */
  roomy: () => boolean;
  /** 挂上时给出输入框的命令式接口。 */
  composerRef?: (api: ComposerApi) => void;
  onSend: (text: string, imageRefs: string[], followUp: boolean) => void;
  onCancel: () => void;
  onForceRestart: () => void;
  onSelectConfig: (configId: string, valueId: string, boolVal?: boolean) => void;
  onCompact: () => void;
  onNewSession?: () => void;
  onPermissionSelect: (toolCallId: string, optionId: string) => void;
  onElicitChoose: (fieldIx: number, optIx: number) => void;
  onElicitText: (fieldIx: number, value: string) => void;
  onElicitSubmit: () => void;
  onElicitDismiss: () => void;
  onDeliverQueuedNow: (text: string) => void;
  onWithdrawPendingPrompt: (id: number) => void;
  onClearQueue: () => void;
  onStopSubagent: (id: string) => void;
  onInspectSubagent: (sub: any) => void;
  onStopBackgroundTask: (id: string) => void;
  onManageCapabilities?: () => void;
  onPreviewAttachment: (att: AttachedImage) => void;
  onFocusIn?: () => void;
}) {
  const look = theme;
  const draft = signal("");
  const attachments = signal<AttachedImage[]>([]);
  const inputHeight = signal(40);
  // 正文档 14/21，跟着界面字号缩放
  const lineHeight = computed(() => (21 / 16) * (look().rem ?? 16));
  const planExpanded = signal(false);
  const workspaceFiles = signal<string[]>([]);
  const filesLoadedCwd = signal<string | null>(null);
  const completionDismissed = signal(false);
  const caretPos = signal(0);

  let inputApi: any = null;
  let lastFocusSeq = 0;

  effect(() => {
    const seq = props.focusSeq();
    if (seq > 0 && seq !== lastFocusSeq) {
      lastFocusSeq = seq;
      inputApi?.focus?.();
    }
  });

  const api: ComposerApi = {
    insert(text) {
      const cur = draft();
      const next = cur ? `${cur}${cur.endsWith(" ") || cur.endsWith("\n") ? "" : " "}${text}` : text;
      draft.set(next);
      caretPos.set(next.length);
      inputApi?.focus?.();
    },
    restore(text, images) {
      if (text) {
        const cur = draft().trim();
        draft.set(cur ? `${text}\n${cur}` : text);
      }
      if (images.length > 0) {
        attachments.set([...attachments(), ...images]);
      }
      inputApi?.focus?.();
    },
  };

  const ensureWorkspaceFiles = () => {
    const cwd = props.cwd();
    const machine = props.machine();
    const cacheKey = cwd ? `${machine ?? ""}\0${cwd}` : null;
    if (!cwd || !cacheKey || filesLoadedCwd() === cacheKey) return;
    filesLoadedCwd.set(cacheKey);
    call(machine, "session.complete_files", { cwd })
      .then((res: any) => {
        if (filesLoadedCwd() !== cacheKey) return;
        const entries = Array.isArray(res?.entries) ? res.entries : [];
        workspaceFiles.set(entries);
      })
      .catch(() => {
        if (filesLoadedCwd() === cacheKey) filesLoadedCwd.set(null);
      });
  };

  const trigger = computed(() => {
    if (completionDismissed()) return null;
    const text = draft();
    const pos = Math.min(text.length, Math.max(0, caretPos()));
    return detectCompletionTrigger(text.slice(0, pos));
  });

  effect(() => {
    if (trigger()?.kind === "at") {
      ensureWorkspaceFiles();
    }
  });

  // 弹层里高亮的那项只对这一次触发有效：输入一变（触发重算）就回到第一项。
  const completionPick = signal<{ trigger: unknown; index: number }>({ trigger: null, index: 0 });
  const completionIndex = computed(() =>
    completionPick().trigger === trigger() ? completionPick().index : 0,
  );

  const completionItems = computed(() =>
    buildCompletionCandidates(
      trigger(),
      workspaceFiles(),
      props.availableCommands(),
      {
        clear: ct("slash.clear"),
        compact: ct("slash.compact"),
        handoff: ct("slash.handoff"),
        shake: ct("slash.shake"),
      },
    ),
  );

  const acceptCompletion = (item: { insert: string }) => {
    const trig = trigger();
    if (!trig) return;
    const text = draft();
    const pos = Math.min(text.length, Math.max(trig.start, caretPos()));
    const next = text.slice(0, trig.start) + item.insert + text.slice(pos);
    const nextCaret = trig.start + item.insert.length;
    draft.set(next);
    caretPos.set(nextCaret);
    completionDismissed.set(false);
    inputApi?.setSelection?.(nextCaret, nextCaret);
    inputApi?.focus?.();
  };

  const acceptSelected = () => {
    const items = completionItems();
    if (items.length === 0) return false;
    acceptCompletion(items[Math.max(0, Math.min(completionIndex(), items.length - 1))]);
    return true;
  };

  // 弹层开着、输入框有焦点时，上下键（macOS 上还有 Ctrl+P/N）、Tab、Esc 归弹层。
  // 输入框自己吃键，onKeyDown 到不了，所以绑在 keymap 上，先于输入框处理。
  const inputFocused = signal(false);
  const completionOpen = computed(() => completionItems().length > 0);
  const emacsKeys = localOs() === "darwin";
  effect(() => {
    if (!inputFocused() || !completionOpen()) return;
    const step = (by: number) => () => {
      const n = completionItems().length;
      if (n > 0) completionPick.set({ trigger: trigger(), index: (completionIndex() + by + n) % n });
    };
    const moves: Record<string, () => void> = { up: step(-1), down: step(1) };
    if (emacsKeys) {
      moves["ctrl+p"] = step(-1);
      moves["ctrl+n"] = step(1);
    }
    const offMoves = keymap.bind(moves, { when: "always", repeat: true });
    const offKeys = keymap.bind(
      { tab: () => void acceptSelected(), escape: () => completionDismissed.set(true) },
      { when: "always" },
    );
    return () => {
      offMoves();
      offKeys();
    };
  });

  const handleSubmit = () => {
    if (props.readOnly() || props.dead()) return;
    if (acceptSelected()) return;
    const text = draft().trim();
    const atts = attachments();
    if (!text && atts.length === 0) return;

    if (isNewConversationCommand(text) && atts.length === 0) {
      draft.set("");
      props.onNewSession?.();
      return;
    }
    if (text.toLowerCase() === "/compact" && atts.length === 0) {
      draft.set("");
      props.onCompact();
      return;
    }

    const refs = atts.map((a) => a.ref_id);
    draft.set("");
    caretPos.set(0);
    attachments.set([]);
    inputHeight.set(40);
    props.onSend(text, refs, false);
  };

  const attachPaths = async (paths: string[]) => {
    for (const raw of paths) {
      if (!raw) continue;
      if (props.supportsImage() && props.machine() == null && IMAGE_EXT_RE.test(raw)) {
        try {
          const meta = await acpPasteImage({ path: raw });
          if (meta?.ref_id) {
            attachments.set([...attachments(), meta]);
            continue;
          }
        } catch {
          // 非图片按路径提及
        }
      }
      const cur = draft();
      const sep = cur && !cur.endsWith(" ") && !cur.endsWith("\n") ? " " : "";
      const next = `${cur}${sep}@${raw} `;
      draft.set(next);
      caretPos.set(next.length);
    }
    inputApi?.focus?.();
  };

  const handlePickFiles = async () => {
    if (props.readOnly() || props.dead()) return;
    try {
      const picked = await run("smelt.app/pick-paths", {
        machine: props.machine() ?? null,
        title: ct("attach.title"),
        confirm: ct("attach.confirm"),
        files: true,
        multiple: true,
      });
      if (Array.isArray(picked) && picked.length > 0) {
        await attachPaths(picked);
      }
    } catch {
      // 用户取消或选择器不可用
    }
  };

  const handlePaste = async (ev: any) => {
    if (props.readOnly()) return;
    try {
      const clip = await acpClipboardImages();
      const imgs: AttachedImage[] = Array.isArray(clip?.images) ? clip.images : [];
      const paths: string[] = Array.isArray(clip?.paths) ? clip.paths : [];
      if (imgs.length > 0 && props.supportsImage()) {
        attachments.set([...attachments(), ...imgs]);
        return;
      }
      if (paths.length > 0) {
        await attachPaths(paths);
        return;
      }
    } catch {
      // 回落到事件自带粘贴内容
    }

    const uris: string[] = Array.isArray(ev?.uris) ? ev.uris : [];
    if (uris.length > 0) {
      await attachPaths(uris.map(fileUriToPath));
      return;
    }

    if (typeof ev?.text === "string" && ev.text.length > 0) {
      const cur = draft();
      const pos = Math.min(cur.length, Math.max(0, caretPos()));
      const next = cur.slice(0, pos) + ev.text + cur.slice(pos);
      const nextCaret = pos + ev.text.length;
      draft.set(next);
      caretPos.set(nextCaret);
      inputApi?.setSelection?.(nextCaret, nextCaret);
    }
  };

  const hasQueue = computed(
    () =>
      props.pendingPrompts().length > 0 ||
      props.queuedSteering().length > 0 ||
      props.queuedFollowUp().length > 0,
  );

  const planSummary = computed(() => {
    const entries: any[] = props.plan()?.entries ?? [];
    if (entries.length === 0) return null;
    const done = entries.filter((e) => e.status === "Completed").length;
    const active = entries.find((e) => e.status === "InProgress");
    return { total: entries.length, done, active: active?.content ?? null, entries };
  });

  const canSend = computed(
    () =>
      !props.readOnly() &&
      !props.dead() &&
      (draft().trim().length > 0 || attachments().length > 0),
  );

  const awaitingUser = computed(
    () => props.permissions().length > 0 || props.elicitation() != null,
  );

  const hasPendingConfig = computed(() => props.pendingConfigCount() > 0 && props.turnActive());

  // 在跑：停止按钮外圈转（和侧栏「在跑＝中性色转圈」同一种表达）；等你时不转，边框换琥珀色。
  const isVisible = () => (props.visible ? props.visible() : true);
  const ringMode = computed(() =>
    stopRingMode({
      turnActive: props.turnActive(),
      awaitingUser: awaitingUser(),
      windowActive: isVisible() && windowActive(),
      reduceMotion: reduceMotion(),
    }),
  );
  const ringMarkup = computed(() => stopRingMarkup(look().palette.text));

  // 材质：光从上面来。高光取主题的 `depth_top`（开关圆钮上沿同一色）压透明度；浅色主题里面就是白的，自然看不出。
  const cardSheen = computed(
    () => `linear-gradient(90deg, transparent, ${colorWithAlpha(look().colors.depth_top, 0.12)}, transparent)`,
  );
  const cardWash = computed(
    () => `linear-gradient(180deg, ${colorWithAlpha(look().colors.depth_top, 0.025)} 0%, transparent 40%)`,
  );

  // 输入框几乎一直有焦点，不另画焦点环；边框只表示等你。
  const cardClass = computed(() =>
    awaitingUser()
      ? "relative w-full rounded-3xl border border-need/45 bg-card shadow-md transition-colors duration-150"
      : "relative w-full rounded-3xl border border-border bg-card shadow-md transition-colors duration-150",
  );

  // 输入框建好以后再交出接口，挂上时就来的文本也能拿到焦点。
  const view = (
    <View
      style={{
        width: "100%",
        paddingHorizontal: 16,
        paddingTop: 4,
        paddingBottom: 16,
        alignItems: "center",
        flexShrink: 0,
      }}
    >
      <View
        style={{
          position: "relative",
          width: "100%",
          maxWidth: 768,
          gap: 8,
        }}
      >
        {/* @ / 补全弹层（锚定在输入框正上方） */}
        <If when={completionItems().length > 0}>
          <View
            class="w-full rounded-xl bg-soft p-1.5 shadow-floating"
            style={{ maxHeight: 244 }}
          >
            <Scroll style={{ maxHeight: 232 }}>
              <View style={{ gap: 2 }}>
                <Each of={completionItems()}>
                  {(item, idx) => {
                    const selected = computed(() => idx() === completionIndex());
                    return (
                      <SmeltRow
                        selected={selected()}
                        onPress={() => acceptCompletion(item())}
                        style={{ height: 30, paddingHorizontal: 10, gap: 8 }}
                      >
                        <Text
                          style={{
                            fontFamily: look().fonts?.mono,
                            fontSize: look().type.text_xsmall.size,
                            color: look().palette.text,
                            numberOfLines: 1,
                          }}
                        >
                          {item().label}
                        </Text>
                        <If when={Boolean(item().hint)}>
                          <Text
                            style={{
                              flex: 1,
                              minWidth: 0,
                              fontSize: look().type.text_xsmall.size,
                              color: look().palette.faint,
                              numberOfLines: 1,
                            }}
                          >
                            {item().hint}
                          </Text>
                        </If>
                      </SmeltRow>
                    );
                  }}
                </Each>
              </View>
            </Scroll>
          </View>
        </If>

        {/* 主输入卡片：面上从上往下一层极淡的受光，上沿一道细高光，读起来是一块立着的板。 */}
          <View class={cardClass()} style={{ backgroundImage: cardWash() }}>
          <View
            style={{
              position: "absolute",
              top: 0,
              left: 24,
              right: 24,
              height: 1,
              backgroundImage: cardSheen(),
              pointerEvents: "none",
            }}
          />

          {/* 1. 权限审批卡 */}
          <PermissionTray
            permissions={props.permissions}
            submitting={props.permissionSubmitting}
            onSelect={props.onPermissionSelect}
          />

          {/* 2. Elicitation 提问表单卡 */}
          <ElicitationCard
            elicitation={props.elicitation}
            onChoose={props.onElicitChoose}
            onText={props.onElicitText}
            onSubmit={props.onElicitSubmit}
            onDismiss={props.onElicitDismiss}
          />

          {/* 3. 可折叠计划条 */}
          <If when={planSummary() != null}>
            <SmeltDisclosure
              open={planExpanded()}
              style={{ borderBottomWidth: 1, borderBottomColor: look().palette.hair }}
              content={
                <View style={{ paddingLeft: 34, paddingRight: 16, paddingBottom: 8, gap: 4 }}>
                  <Each of={planSummary()?.entries ?? []}>
                    {(step) => (
                      <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
                        <SmeltIcon
                          path={
                            step().status === "Completed"
                              ? "icons/check.svg"
                              : step().status === "InProgress"
                                ? "icons/loader.svg"
                                : "icons/clock.svg"
                          }
                          size={12}
                          color={
                            step().status === "Completed"
                              ? look().palette.ok
                              : step().status === "InProgress"
                                ? look().palette.muted
                                : look().palette.faint
                          }
                        />
                        <Text
                          style={{
                            flex: 1,
                            fontSize: look().type.text_xsmall.size,
                            color:
                              step().status === "Completed"
                                ? look().palette.faint
                                : look().palette.text,
                          }}
                        >
                          {step().content}
                        </Text>
                      </View>
                    )}
                  </Each>
                </View>
              }
            >
              <SmeltRow
                rounded={false}
                onPress={() => planExpanded.set(!planExpanded())}
                style={{ paddingHorizontal: 16, paddingVertical: 8, gap: 6 }}
              >
                <SmeltDisclosureChevron
                  open={planExpanded()}
                  size={12}
                  color={look().palette.faint}
                />
                <Text
                  style={{
                    fontSize: look().type.text_xsmall_medium.size,
                    fontWeight: look().type.text_xsmall_medium.weight,
                    color: look().palette.muted,
                  }}
                >
                  {ct("plan.title")} ({planSummary()?.done ?? 0}/{planSummary()?.total ?? 0})
                </Text>
                <If when={Boolean(planSummary()?.active)}>
                  <Text
                    style={{
                      flex: 1,
                      minWidth: 0,
                      fontSize: look().type.text_xsmall.size,
                      color: look().palette.text,
                      numberOfLines: 1,
                    }}
                  >
                    · {planSummary()?.active ?? ""}
                  </Text>
                </If>
              </SmeltRow>
            </SmeltDisclosure>
          </If>

          {/* 4. 排队中消息条 */}
          <If when={hasQueue()}>
            <View
              style={{
                borderBottomWidth: 1,
                borderBottomColor: look().palette.hair,
                paddingHorizontal: 16,
                paddingVertical: 8,
                gap: 6,
              }}
            >
              <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between" }}>
                <Text
                  style={{
                    fontSize: look().type.text_xsmall_medium.size,
                    fontWeight: look().type.text_xsmall_medium.weight,
                    color: look().palette.muted,
                  }}
                >
                  {ct("queue.title")}
                </Text>
                <If when={props.queuedSteering().length > 0 || props.queuedFollowUp().length > 0}>
                  <SmeltButton variant="quiet" size="sm" label={ct("queue.clear")} onPress={props.onClearQueue} />
                </If>
              </View>
              <Each of={props.pendingPrompts()}>
                {(p) => (
                  <View class="flex-row items-center gap-2 px-2.5 py-1 rounded-lg bg-foreground/[0.08]">
                    <Text
                      style={{
                        flex: 1,
                        minWidth: 0,
                        fontSize: look().type.text_xsmall.size,
                        color: look().palette.text,
                        numberOfLines: 1,
                      }}
                    >
                      {p().text}
                    </Text>
                    <If when={Boolean(p().withdrawable)}>
                      <SmeltButton
                        variant="quiet"
                        size="sm"
                        label={ct("queue.withdraw")}
                        onPress={() => props.onWithdrawPendingPrompt(p().id)}
                      />
                    </If>
                  </View>
                )}
              </Each>
              <Each of={props.queuedSteering()}>
                {(txt) => (
                  <View class="flex-row items-center gap-2 px-2.5 py-1 rounded-lg bg-foreground/[0.08]">
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                      {ct("queue.current_turn")} ·
                    </Text>
                    <Text
                      style={{
                        flex: 1,
                        minWidth: 0,
                        fontSize: look().type.text_xsmall.size,
                        color: look().palette.text,
                        numberOfLines: 1,
                      }}
                    >
                      {txt()}
                    </Text>
                  </View>
                )}
              </Each>
              <Each of={props.queuedFollowUp()}>
                {(txt) => (
                  <View class="flex-row items-center gap-2 px-2.5 py-1 rounded-lg bg-foreground/[0.08]">
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                      {ct("queue.next_turn")} ·
                    </Text>
                    <Text
                      style={{
                        flex: 1,
                        minWidth: 0,
                        fontSize: look().type.text_xsmall.size,
                        color: look().palette.text,
                        numberOfLines: 1,
                      }}
                    >
                      {txt()}
                    </Text>
                    <SmeltButton
                      variant="quiet"
                      size="sm"
                      label={ct("queue.send_now")}
                      onPress={() => props.onDeliverQueuedNow(txt())}
                    />
                  </View>
                )}
              </Each>
            </View>
          </If>

          {/* 5. 已附加的图片缩略图 */}
          <If when={attachments().length > 0}>
            <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8, paddingHorizontal: 16, paddingTop: 12 }}>
              <Each of={attachments()}>
                {(att, idx) => (
                  <View
                    style={{
                      position: "relative",
                      borderRadius: look().radius.xl,
                      borderWidth: 1,
                      borderColor: look().palette.hair,
                      overflow: "hidden",
                      backgroundColor: look().palette.bg,
                    }}
                  >
                    <SmeltRow rounded={false} onPress={() => props.onPreviewAttachment(att())}>
                      <Image
                        src={att().preview_uri}
                        style={{ width: 64, height: 64, objectFit: "cover" }}
                      />
                    </SmeltRow>
                    <View style={{ position: "absolute", top: 3, right: 3 }}>
                      <SmeltButton
                        variant="floating"
                        size="xs"
                        shape="circle"
                        icon="icons/close.svg"
                        onPress={() => {
                          const i = idx();
                          attachments.set(attachments().filter((_, k) => k !== i));
                        }}
                      />
                    </View>
                  </View>
                )}
              </Each>
            </View>
          </If>

          {/* 6. 多行自适应正文输入区（14px 正文档，舒适留白） */}
          <View style={{ paddingHorizontal: 18, paddingTop: 14, paddingBottom: 6 }}>
            <Input
              ref={(api: any) => {
                inputApi = api;
              }}
              multiline
              submitBehavior="submit"
              pasteBehavior="report"
              readonly={props.readOnly() || props.dead()}
              value={draft()}
              placeholder={
                props.readOnly()
                  ? ct("composer.placeholder_readonly")
                  : props.turnActive()
                    ? ct("composer.placeholder_running")
                    : ct("composer.placeholder")
              }
              onFocus={() => {
                inputFocused.set(true);
                props.onFocusIn?.();
              }}
              onBlur={() => inputFocused.set(false)}
              onChange={({ value }: any) => {
                draft.set(value);
                caretPos.set(value.length);
                completionDismissed.set(false);
              }}
              onSelectionChange={({ end }: any) => {
                if (typeof end === "number") {
                  caretPos.set(end);
                }
              }}
              onContentSizeChange={({ size }: any) => {
                if (size?.height) {
                  const next = Math.max(40, Math.min(180, Math.ceil(size.height)));
                  if (next !== inputHeight()) inputHeight.set(next);
                }
              }}
              onSubmit={handleSubmit}
              onPaste={(ev: any) => void handlePaste(ev)}
              onKeyPress={(e: any) => {
                if (e?.key === "Escape" && props.turnActive() && !draft().trim()) {
                  props.onCancel();
                }
              }}
              style={{
                width: "100%",
                height: Math.max(props.roomy() ? ROOMY_INPUT_LINES * lineHeight() : 0, inputHeight()),
                fontSize: (14 / 16) * (look().rem ?? 16),
                lineHeight: lineHeight(),
                color: look().palette.text,
                placeholderColor: look().palette.faint,
              }}
            />
          </View>

          {/* 7. 底部工具栏：左边往这条消息里加东西（附件、模型、能力），右边是状态（用量）和唯一的主操作 */}
          <View
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 2,
              paddingLeft: 10,
              paddingRight: 10,
              paddingTop: 2,
              paddingBottom: 10,
            }}
          >
            <If when={!props.readOnly() && !props.dead()}>
              <SmeltButton
                variant="quiet"
                shape="circle"
                icon="icons/plus.svg"
                tooltip={ct("action.attach")}
                onPress={() => void handlePickFiles()}
              />
            </If>

            <ModelConfigPopover
              model={props.model}
              configs={props.configs}
              fallbackLabel={props.fallbackModelLabel}
              hasPendingNotice={hasPendingConfig}
              onSelectConfig={props.onSelectConfig}
            />

            <CapabilitiesPopover
              skills={props.skills}
              mcpServers={props.mcpServers}
              byAgent={props.capabilitiesByAgent}
              onUseSkill={(command) => {
                const prefix = `/${command} `;
                draft.set(draft() ? `${draft()} ${prefix}` : prefix);
                inputApi?.focus?.();
              }}
              onManage={props.onManageCapabilities}
            />

            <SubagentsControl
              subagents={props.subagents}
              onStop={props.onStopSubagent}
              onInspect={props.onInspectSubagent}
            />

            <BackgroundTasksControl
              tasks={props.backgroundTasks}
              onStop={props.onStopBackgroundTask}
            />

            <View style={{ flex: 1 }} />

            <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
              <UsagePopover
                usage={props.usage}
                breakdown={props.usageBreakdown}
                compacting={props.compacting}
                visible={isVisible}
                supportsCompaction={props.supportsCompaction}
                onCompact={props.onCompact}
              />

              {/* 主操作始终唯一：运行中是停止，空闲是发送 */}
              <If
                when={props.turnActive()}
                else={
                  <If when={!props.readOnly()}>
                    <SmeltButton
                      variant="primary"
                      shape="circle"
                      disabled={!canSend()}
                      icon="icons/arrow-up.svg"
                      tooltip={ct("action.send")}
                      onPress={handleSubmit}
                    />
                  </If>
                }
              >
                <View style={{ width: 28, height: 28 }}>
                  <If when={ringMode() !== "off"}>
                    <SmeltVector
                      markup={ringMarkup()}
                      width={STOP_RING_SIZE}
                      height={STOP_RING_SIZE}
                      class={ringMode() === "spin" ? "animate-spin" : undefined}
                      style={{ position: "absolute", left: -STOP_RING_INSET, top: -STOP_RING_INSET }}
                    />
                  </If>
                  <SmeltButton
                    variant="primary"
                    shape="circle"
                    icon="icons/stop.svg"
                    tooltip={ct("action.stop")}
                    onPress={props.onCancel}
                  />
                </View>
              </If>
            </View>
          </View>
        </View>
      </View>
    </View>
  );
  props.composerRef?.(api);
  return view;
}
