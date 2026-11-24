// 对话底部输入栏：权限审批卡 / Elicitation 表单 / 计划栏 / 排队提示 / 附件缩略图 / `@`/`/` 补全弹层 /
// 多行自适应输入框（Enter 发送、Shift+Enter 换行、零拷贝截图粘贴、+ 选文件）/ 模型、能力、子智能体、后台任务与用量工具栏。
import {
  Animated,
  AnimatedValue,
  cleanup,
  computed,
  Easing,
  effect,
  Image,
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
  run,
  SmeltButton,
  SmeltIcon,
  theme,
  windowActive,
} from "smelt/sdk";
import { ElicitationCard, PermissionTray } from "./cards.tsx";
import { ct } from "./i18n.ts";
import {
  buildCompletionCandidates,
  colorWithAlpha,
  composerSweepMode,
  detectCompletionTrigger,
  isNewConversationCommand,
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

const IMAGE_EXT_RE = /\.(png|jpe?g|gif|webp)$/i;

export function Composer(props: {
  machine: () => string | null;
  cwd: () => string | null;
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
  skills: () => any[];
  mcpServers: () => any[];
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
  insertText: () => { seq: number; text: string } | null;
  restoreDraft: () => { seq: number; text: string; images: AttachedImage[] } | null;
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
  const focused = signal(false);
  const planExpanded = signal(false);
  const workspaceFiles = signal<string[]>([]);
  const filesLoadedCwd = signal<string | null>(null);
  const completionIndex = signal(0);
  const completionDismissed = signal(false);
  const caretPos = signal(0);

  let inputApi: any = null;
  let lastFocusSeq = 0;
  let lastInsertSeq = 0;
  let lastRestoreSeq = 0;

  effect(() => {
    const seq = props.focusSeq();
    if (seq > 0 && seq !== lastFocusSeq) {
      lastFocusSeq = seq;
      inputApi?.focus?.();
    }
  });

  effect(() => {
    const req = props.insertText();
    if (req && req.seq !== lastInsertSeq) {
      lastInsertSeq = req.seq;
      const cur = draft();
      const next = cur
        ? `${cur}${cur.endsWith(" ") || cur.endsWith("\n") ? "" : " "}${req.text}`
        : req.text;
      draft.set(next);
      caretPos.set(next.length);
      inputApi?.focus?.();
    }
  });

  effect(() => {
    const req = props.restoreDraft();
    if (req && req.seq !== lastRestoreSeq) {
      lastRestoreSeq = req.seq;
      if (req.text) {
        const cur = draft().trim();
        draft.set(cur ? `${req.text}\n${cur}` : req.text);
      }
      if (req.images.length > 0) {
        attachments.set([...attachments(), ...req.images]);
      }
      inputApi?.focus?.();
    }
  });

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
    completionIndex.set(0);
  });

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

  const handleSubmit = () => {
    if (props.readOnly() || props.dead()) return;
    const items = completionItems();
    if (items.length > 0) {
      const idx = Math.max(0, Math.min(completionIndex(), items.length - 1));
      acceptCompletion(items[idx]);
      return;
    }
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

  // 顶边运行态扫光状态与原生 UI 线程动画驱动（仅在激活时运行，空闲与失焦/减弱动态零唤醒）
  const INITIAL_TRACK_WIDTH = 720;
  const computeBeamWidth = (w: number) => Math.max(96, Math.round(w * 0.34));
  const sweepMode = computed(() =>
    composerSweepMode({
      turnActive: props.turnActive(),
      awaitingUser: awaitingUser(),
      windowActive: windowActive(),
      reduceMotion: reduceMotion(),
    }),
  );
  const sweepMounted = signal(false);
  const trackWidth = signal(INITIAL_TRACK_WIDTH);
  const beamWidth = computed(() => computeBeamWidth(trackWidth()));
  const haloLeft = computed(() => Math.round(beamWidth() * 0.22));
  const haloWidth = computed(() => Math.round(beamWidth() * 0.56));

  const presence = new AnimatedValue(0);
  const sweepPos = new AnimatedValue(0);
  const beamTravelVal = new AnimatedValue(
    INITIAL_TRACK_WIDTH - computeBeamWidth(INITIAL_TRACK_WIDTH),
  );
  const beamX = sweepPos.mul(beamTravelVal);

  let activeLoop: any = null;
  let fadeToken = 0;
  const stopLoop = () => {
    if (activeLoop) {
      activeLoop.stop();
      activeLoop = null;
    }
  };
  const handleTrackLayout = (ev: any) => {
    const w = Math.round(ev?.bounds?.size?.width ?? 0);
    if (w > 0 && w !== trackWidth()) {
      trackWidth.set(w);
      beamTravelVal.set(Math.max(0, w - computeBeamWidth(w)));
    }
  };

  effect(() => {
    const mode = sweepMode();
    if (mode === "off") {
      if (!sweepMounted()) {
        stopLoop();
        return;
      }
      const token = ++fadeToken;
      void presence
        .timing(0, { duration: 280, easing: Easing.out(Easing.cubic) })
        .then(({ finished }) => {
          if (token === fadeToken && finished && sweepMode() === "off") {
            stopLoop();
            sweepMounted.set(false);
          }
        });
      return;
    }

    const wasMounted = sweepMounted();
    fadeToken += 1;
    if (!wasMounted) {
      sweepMounted.set(true);
    }

    if (mode === "sweep") {
      if (!activeLoop) {
        activeLoop = Animated.keyframes(sweepPos, {
          keyframes: [0, 1, 0],
          duration: 4400,
          easing: Easing.inOut(Easing.sin),
          iterations: "infinite",
        });
        void activeLoop.start();
      }
    } else if (mode === "hold") {
      stopLoop();
      if (!wasMounted) {
        sweepPos.set(0.5);
      } else {
        void sweepPos.timing(0.5, { duration: 260, easing: Easing.out(Easing.cubic) });
      }
    } else {
      stopLoop();
    }

    void presence.timing(1, { duration: 220, easing: Easing.out(Easing.cubic) });
  });

  cleanup(() => {
    fadeToken += 1;
    stopLoop();
    void presence.stop();
    void sweepPos.stop();
  });

  const sweepColor = computed(() => (awaitingUser() ? look().palette.need : look().palette.run));
  const sweepHaloColor = computed(() => colorWithAlpha(sweepColor(), 0.35));

  const cardClass = computed(() => {
    if (focused()) {
      return "relative w-full rounded-3xl border border-ring ring-2 ring-ring/25 bg-card shadow-md overflow-hidden transition-colors duration-150";
    }
    if (awaitingUser()) {
      return "relative w-full rounded-3xl border border-need/45 bg-card shadow-md overflow-hidden transition-colors duration-150";
    }
    if (props.turnActive()) {
      return "relative w-full rounded-3xl border border-run/35 bg-card shadow-md overflow-hidden transition-colors duration-150";
    }
    return "relative w-full rounded-3xl border border-border bg-card shadow-md overflow-hidden transition-colors duration-150";
  });

  return (
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
            class="w-full rounded-xl border border-border bg-popover p-1.5 shadow-lg overflow-hidden"
            style={{ maxHeight: 244 }}
          >
            <Scroll style={{ maxHeight: 232 }}>
              <View style={{ gap: 2 }}>
                <Each of={completionItems()}>
                  {(item, idx) => {
                    const selected = computed(() => idx() === completionIndex());
                    return (
                      <View
                        onPress={() => acceptCompletion(item())}
                        class="hover:bg-accent"
                        style={{
                          height: 30,
                          paddingHorizontal: 10,
                          borderRadius: look().radius.row ?? 6,
                          flexDirection: "row",
                          alignItems: "center",
                          gap: 8,
                          backgroundColor: selected() ? look().palette.row_selected : "transparent",
                        }}
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
                      </View>
                    );
                  }}
                </Each>
              </View>
            </Scroll>
          </View>
        </If>

        {/* 主输入胶囊卡片及其外部运行态顶边扫光轨道（避免光晕被 overflow-hidden 裁剪） */}
        <View style={{ position: "relative", width: "100%" }}>
          <View class={cardClass()}>

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
            <View
              style={{
                borderBottomWidth: 1,
                borderBottomColor: look().palette.hair,
                paddingHorizontal: 16,
                paddingVertical: 8,
                gap: 6,
              }}
            >
              <View
                onPress={() => planExpanded.set(!planExpanded())}
                style={{ flexDirection: "row", alignItems: "center", gap: 6 }}
              >
                <SmeltIcon
                  path={planExpanded() ? "icons/chevron-down.svg" : "icons/chevron-right.svg"}
                  size={12}
                  color={look().palette.faint}
                />
                <Text
                  style={{
                    fontSize: look().type.text_xsmall_medium?.size ?? 12,
                    fontWeight: look().type.text_xsmall_medium?.weight ?? 500,
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
              </View>
              <If when={planExpanded()}>
                <View style={{ paddingLeft: 18, gap: 4 }}>
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
                                ? look().palette.run
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
              </If>
            </View>
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
                    fontSize: look().type.text_xsmall_medium?.size ?? 12,
                    fontWeight: look().type.text_xsmall_medium?.weight ?? 500,
                    color: look().palette.muted,
                  }}
                >
                  {ct("queue.title")}
                </Text>
                <If when={props.queuedSteering().length > 0 || props.queuedFollowUp().length > 0}>
                  <SmeltButton quiet small label={ct("queue.clear")} onPress={props.onClearQueue} />
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
                        quiet
                        small
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
                      quiet
                      small
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
                      borderRadius: 10,
                      borderWidth: 1,
                      borderColor: look().palette.hair,
                      overflow: "hidden",
                      backgroundColor: look().palette.bg,
                    }}
                  >
                    <View onPress={() => props.onPreviewAttachment(att())}>
                      <Image
                        src={att().preview_uri}
                        style={{ width: 64, height: 64, objectFit: "cover" }}
                      />
                    </View>
                    <View style={{ position: "absolute", top: 3, right: 3 }}>
                      <SmeltButton
                        dismiss
                        small
                        icon="icons/close.svg"
                        style={{
                          backgroundColor: look().palette.surface,
                          borderRadius: 999,
                        }}
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
                focused.set(true);
                props.onFocusIn?.();
              }}
              onBlur={() => focused.set(false)}
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
              onKeyDown={(ev: any) => {
                const key = ev?.key;
                const items = completionItems();
                if (items.length > 0) {
                  if (key === "ArrowDown") {
                    completionIndex.set((completionIndex() + 1) % items.length);
                    return;
                  }
                  if (key === "ArrowUp") {
                    completionIndex.set((completionIndex() - 1 + items.length) % items.length);
                    return;
                  }
                  if (key === "Tab") {
                    const idx = Math.max(0, Math.min(completionIndex(), items.length - 1));
                    acceptCompletion(items[idx]);
                    return;
                  }
                  if (key === "Escape") {
                    completionDismissed.set(true);
                    return;
                  }
                }
                if (key === "Escape" && props.turnActive() && !draft().trim()) {
                  props.onCancel();
                }
              }}
              style={{
                width: "100%",
                height: inputHeight(),
                fontSize: (14 / 16) * (look().rem ?? 16),
                lineHeight: (21 / 16) * (look().rem ?? 16),
                color: look().palette.text,
                placeholderColor: look().palette.faint,
              }}
            />
          </View>

          {/* 7. 底部工具栏：左侧模型与能力胶囊，右侧用量、+ 附件与唯一圆形主操作钮 */}
          <View
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 4,
              paddingLeft: 12,
              paddingRight: 12,
              paddingTop: 2,
              paddingBottom: 10,
            }}
          >
            <ModelConfigPopover
              model={props.model}
              configs={props.configs}
              fallbackLabel={props.fallbackModelLabel}
              turnActive={props.turnActive}
              hasPendingNotice={hasPendingConfig}
              onSelectConfig={props.onSelectConfig}
            />

            <CapabilitiesPopover
              skills={props.skills}
              mcpServers={props.mcpServers}
              byAgent={props.capabilitiesByAgent}
              onUseSkill={(name) => {
                const prefix = `/${name} `;
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
                supportsCompaction={props.supportsCompaction}
                onCompact={props.onCompact}
              />

              <If when={!props.readOnly() && !props.dead()}>
                <SmeltButton
                  quiet
                  circle
                  icon="icons/plus.svg"
                  tooltip={ct("action.attach")}
                  onPress={() => void handlePickFiles()}
                />
              </If>

              {/* 主操作始终唯一：运行中是停止，空闲是发送 */}
              <If
                when={props.turnActive()}
                else={
                  <If when={!props.readOnly()}>
                    <SmeltButton
                      primary
                      circle
                      disabled={!canSend()}
                      icon="icons/arrow-up.svg"
                      tooltip={ct("action.send")}
                      onPress={handleSubmit}
                    />
                  </If>
                }
              >
                <SmeltButton
                  primary
                  circle
                  icon="icons/stop.svg"
                  tooltip={ct("action.stop")}
                  onPress={props.onCancel}
                />
              </If>
            </View>
          </View>
          </View>

          <If when={sweepMounted()}>
            <View
              onLayout={handleTrackLayout}
              style={{
                position: "absolute",
                top: 0,
                left: 24,
                right: 24,
                height: 2,
                opacity: presence,
                pointerEvents: "none",
              }}
            >
              {/* 全宽淡底轨（减弱动态 / 失焦时保持柔和静显） */}
              <View
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  right: 0,
                  height: 1,
                  borderRadius: 1,
                  opacity: sweepMode() === "still" ? 0.5 : 0.26,
                  backgroundImage: `linear-gradient(90deg, transparent, ${sweepColor()}, transparent)`,
                }}
              />
              {/* 彗星扫光束 + 柔和光晕 */}
              <If when={sweepMode() !== "still"}>
                <View
                  style={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: beamWidth(),
                    height: 2,
                    transform: [{ translateX: beamX }],
                  }}
                >
                  <View
                    style={{
                      position: "absolute",
                      top: 0,
                      left: haloLeft(),
                      width: haloWidth(),
                      height: 2,
                      borderRadius: 999,
                      boxShadow: [{ x: 0, y: 0, blur: 10, spread: 1, color: sweepHaloColor() }],
                    }}
                  />
                  <View
                    style={{
                      width: "100%",
                      height: 2,
                      borderRadius: 1,
                      backgroundImage: `linear-gradient(90deg, transparent, ${sweepColor()}, transparent)`,
                    }}
                  />
                </View>
              </If>
            </View>
          </If>
        </View>
      </View>
    </View>
  );
}
