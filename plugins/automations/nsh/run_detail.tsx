// 选中运行的右侧详情页：支持挂载实时对话现场，以及落库结果、执行过程（含真实 SmeltDiffView）、输入与元数据。
import { computed, Scroll, Text, View } from "nsh";
import {
  SmeltButton,
  SmeltDiffView,
  SmeltDisclosure,
  SmeltDisclosureChevron,
  SmeltIcon,
  SmeltMarkdown,
  SmeltRow,
  stableMemo,
  t,
} from "smelt/sdk";
import { SmeltConversation } from "smelt/conversation";
import {
  classifyAcpEntry,
  classifyToolOutputPart,
  compactToolHeadline,
  editorLanguageForPath,
  estimateDiffHeight,
  fencedBlock,
  finalReplyIndex,
  formatShellCommand,
  isRunStatusActive,
  isShellAction,
  makeRunDetailKey,
  newSessionLabel,
  runErrorText,
  runMetadataRows,
  runSourceLabel,
  runStatusLabel,
  runSummaryInfo,
  sameRunDetailKey,
  taskNoteSummary,
  toolProcessIconPath,
  transcriptSummary,
  turnFailedText,
} from "../model.js";
import {
  cancelAutomationRun,
  currentTheme,
  liveRunStatusFor,
  pageError,
  retryRunDetail,
  runDetail,
  runDetailError,
  runDetailLoading,
  type RunDetailState,
  toggleRunDetailBlock,
} from "./state.ts";
import { ContentCard, ErrorAlert, StatusDot, runStatusColor } from "./ui.tsx";

const BLOCK_PROCESS = "process";
const BLOCK_INPUT = "input";
const BLOCK_INSTRUCTIONS = "instructions";
const BLOCK_META = "meta";

function DetailDisclosure(props: {
  blockKey: string;
  title: string;
  meta?: string | null;
  expanded: boolean;
  children?: any;
}) {
  const look = currentTheme;
  return (
    <SmeltDisclosure
      open={props.expanded}
      style={{ width: "100%" }}
      content={<View style={{ width: "100%", paddingTop: 8 }}>{props.children}</View>}
    >
      <SmeltRow
        testID={`automation-run-block-${props.blockKey}`}
        accessibilityLabel={props.title}
        onPress={() => toggleRunDetailBlock(props.blockKey)}
        style={{
          width: "100%",
          height: 24,
          marginLeft: -6,
          marginRight: -6,
          paddingLeft: 6,
          paddingRight: 6,
          gap: 6,
        }}
      >
        <SmeltDisclosureChevron open={props.expanded} size={12} color={look().palette.faint} />
        <Text
          style={{
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.text,
          }}
        >
          {props.title}
        </Text>
        <If when={Boolean(props.meta)}>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.faint,
            }}
          >
            {props.meta ?? ""}
          </Text>
        </If>
      </SmeltRow>
    </SmeltDisclosure>
  );
}

/**
 * Bug Fix #9：结构化渲染工具输出，Diff 走一等 Rust `<SmeltDiffView>`，
 * 文本与终端输出仍用动态反引号围栏包裹交由 `<SmeltMarkdown>` 渲染。
 */
function ToolOutputPartsView(props: {
  parts: any[];
  cwd: string | null;
  machine: string | null;
}) {
  const look = currentTheme;
  const items = computed(() =>
    (props.parts ?? [])
      .map((raw, idx) => ({ idx, parsed: classifyToolOutputPart(raw) }))
      .filter(({ parsed }) => {
        if (parsed.type === "text") return parsed.text.trimEnd().length > 0;
        if (parsed.type === "terminal") {
          return (
            parsed.output.trimEnd().length > 0 ||
            parsed.truncated ||
            (parsed.exitCode != null && parsed.exitCode !== 0)
          );
        }
        return parsed.type === "diff" || parsed.type === "image";
      }),
  );

  return (
    <If
      when={items().length > 0}
      else={
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
          {t("run.no_output")}
        </Text>
      }
    >
      <View style={{ width: "100%", minWidth: 0, display: "flex", flexDirection: "column", gap: 8 }}>
        <Each of={items()} key={(it: any) => String(it.idx)}>
          {(item: () => { idx: number; parsed: any }) => {
            const p = () => item().parsed;
            return (
              <View style={{ width: "100%", minWidth: 0, display: "flex", flexDirection: "column", gap: 4 }}>
                <If when={p().type === "text"}>
                  <SmeltMarkdown
                    markdown={fencedBlock("", p().text.trimEnd(), t)}
                    cwd={props.cwd}
                    machine={props.machine}
                  />
                </If>

                <If when={p().type === "terminal"}>
                  <If when={p().output.trimEnd().length > 0}>
                    <SmeltMarkdown
                      markdown={fencedBlock("", p().output.trimEnd(), t)}
                      cwd={props.cwd}
                      machine={props.machine}
                    />
                  </If>
                  <If when={Boolean(p().truncated)}>
                    <Text
                      style={{
                        fontSize: look().type.text_xsmall.size,
                        color: look().palette.faint,
                      }}
                    >
                      {t("run.output_truncated")}
                    </Text>
                  </If>
                  <If when={p().exitCode != null && p().exitCode !== 0}>
                    <Text
                      style={{
                        fontSize: look().type.text_xsmall.size,
                        color: look().palette.bad,
                      }}
                    >
                      {t("run.exit_code", { code: p().exitCode })}
                    </Text>
                  </If>
                </If>

                <If when={p().type === "diff"}>
                  <View
                    style={{
                      width: "100%",
                      minWidth: 0,
                      borderRadius: look().radius.md,
                      borderWidth: 1,
                      borderColor: look().palette.hair,
                      backgroundColor: look().palette.surface,
                      overflow: "hidden",
                      display: "flex",
                      flexDirection: "column",
                    }}
                  >
                    <If when={Boolean(p().path)}>
                      <View
                        style={{
                          paddingLeft: 10,
                          paddingRight: 10,
                          paddingTop: 6,
                          paddingBottom: 6,
                          borderBottomWidth: 1,
                          borderBottomColor: look().palette.hair,
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          gap: 6,
                        }}
                      >
                        <SmeltIcon path="icons/file.svg" size={12} color={look().palette.muted} />
                        <Text
                          style={{
                            flex: 1,
                            minWidth: 0,
                            fontFamily: look().fonts?.mono,
                            fontSize: look().type.text_xsmall.size,
                            color: look().palette.muted,
                            numberOfLines: 1,
                          }}
                        >
                          {p().path}
                        </Text>
                      </View>
                    </If>
                    <View
                      style={{
                        width: "100%",
                        height: estimateDiffHeight(p().oldText, p().newText),
                        display: "flex",
                        flexDirection: "column",
                      }}
                    >
                      <SmeltDiffView
                        oldValue={p().oldText}
                        newValue={p().newText}
                        language={editorLanguageForPath(p().path)}
                        layout="unified"
                        lineNumbers={true}
                        softWrap={true}
                        fontSize={12}
                        lineHeight={18}
                      />
                    </View>
                  </View>
                </If>

                <If when={p().type === "image"}>
                  <Text
                    style={{
                      fontSize: look().type.text_xsmall.size,
                      color: look().palette.faint,
                    }}
                  >
                    {t("run.image_not_saved", { mime: p().mime })}
                  </Text>
                </If>
              </View>
            );
          }}
        </Each>
      </View>
    </If>
  );
}

function TranscriptStep(props: {
  stepKey: string;
  icon: string;
  headline: string;
  flag?: "failed" | "unfinished" | null;
  expandable: boolean;
  expanded: boolean;
  children?: any;
}) {
  const look = currentTheme;
  const ink = () => (props.flag === "failed" ? look().palette.bad : look().palette.muted);
  const flagLabel = () =>
    props.flag === "failed"
      ? t("run_status.failed")
      : props.flag === "unfinished"
        ? t("run.unfinished")
        : "";
  const flagColor = () =>
    props.flag === "failed" ? look().palette.bad : look().palette.faint;

  // 可展开的步骤整行可点；不可展开的只是一行字，不做成按钮（不进 Tab 序、无悬停）。
  const stepRowStyle = {
    width: "100%",
    minHeight: 24,
    marginLeft: -6,
    marginRight: -6,
    paddingLeft: 6,
    paddingRight: 6,
    display: "flex",
    flexDirection: "row",
    alignItems: "center",
    gap: 8,
  } as const;
  const StepHead = () => (
    <>
      <SmeltIcon path={props.icon} size={12} color={ink()} />
      <Text
        style={{
          flex: 1,
          minWidth: 0,
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
          color: ink(),
          numberOfLines: 1,
        }}
      >
        {props.headline}
      </Text>
      <If when={props.flag != null}>
        <Text
          style={{
            flexShrink: 0,
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: flagColor(),
          }}
        >
          {flagLabel()}
        </Text>
      </If>
      <If when={props.expandable}>
        <SmeltDisclosureChevron open={props.expanded} size={12} color={look().palette.faint} />
      </If>
    </>
  );

  return (
    <SmeltDisclosure
      open={props.expandable && props.expanded}
      style={{ width: "100%" }}
      content={
        <View
          style={{
            width: "100%",
            minWidth: 0,
            paddingLeft: 20,
            paddingTop: 4,
            paddingBottom: 8,
          }}
        >
          {props.children}
        </View>
      }
    >
      <If
        when={props.expandable}
        else={
          <View testID={`automation-run-step-${props.stepKey}`} style={stepRowStyle}>
            <StepHead />
          </View>
        }
      >
        <SmeltRow
          testID={`automation-run-step-${props.stepKey}`}
          accessibilityLabel={props.headline}
          onPress={() => toggleRunDetailBlock(props.stepKey)}
          style={stepRowStyle}
        >
          <StepHead />
        </SmeltRow>
      </If>
    </SmeltDisclosure>
  );
}

function TranscriptList(props: {
  entries: any[];
  path: string;
  finalReply: number | null;
  detail: RunDetailState;
  cwd: string | null;
  machine: string | null;
}) {
  const look = currentTheme;
  const rows = computed(() =>
    (props.entries ?? []).map((raw, ix) => ({
      ix,
      key: `${props.path}${ix}`,
      item: classifyAcpEntry(raw),
    })),
  );

  return (
    <View style={{ width: "100%", minWidth: 0, display: "flex", flexDirection: "column", gap: 4 }}>
      <Each of={rows()} key={(r: any) => r.key}>
        {(row: () => { ix: number; key: string; item: any }) => {
          const it = () => row().item;
          const stepKey = () => row().key;
          const expanded = () => props.detail.expanded.has(stepKey());

          return (
            <View style={{ width: "100%", minWidth: 0 }}>
              {/* User / UserWithImages */}
              <If when={it().type === "user"}>
                <View style={{ paddingTop: 4, paddingBottom: 4, display: "flex", flexDirection: "column", gap: 4 }}>
                  <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                    {t("run.user_input")}
                  </Text>
                  <ContentCard style={{ padding: 12 }}>
                    <SmeltMarkdown
                      markdown={(() => {
                        const base = it().text.trim();
                        if (it().images > 0) {
                          const imgNote = t("run.images_not_saved", { count: it().images });
                          return base ? `${base}\n\n${imgNote}` : imgNote;
                        }
                        return base;
                      })()}
                      cwd={props.cwd}
                      machine={props.machine}
                    />
                  </ContentCard>
                </View>
              </If>

              {/* Assistant final reply folded */}
              <If
                when={
                  it().type === "assistant" &&
                  !it().thought &&
                  props.finalReply === row().ix
                }
              >
                <View style={{ paddingTop: 4, paddingBottom: 4 }}>
                  <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                    {t("run.final_reply")}
                  </Text>
                </View>
              </If>

              {/* Assistant reply */}
              <If
                when={
                  it().type === "assistant" &&
                  !it().thought &&
                  props.finalReply !== row().ix &&
                  it().text.trim().length > 0
                }
              >
                <View style={{ paddingTop: 4, paddingBottom: 4 }}>
                  <SmeltMarkdown
                    markdown={it().text}
                    cwd={props.cwd}
                    machine={props.machine}
                  />
                </View>
              </If>

              {/* Assistant thought */}
              <If when={it().type === "assistant" && it().thought && it().text.trim().length > 0}>
                <TranscriptStep
                  stepKey={stepKey()}
                  icon={toolProcessIconPath("think")}
                  headline={t("run.thinking", {
                    preview: it().text.trim().split(/\r?\n/)[0] ?? "",
                  })}
                  expandable={true}
                  expanded={expanded()}
                >
                  <Text
                    style={{
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      color: look().palette.muted,
                    }}
                  >
                    {it().text.trim()}
                  </Text>
                </TranscriptStep>
              </If>

              {/* ToolCall */}
              <If when={it().type === "tool_call"}>
                <TranscriptStep
                  stepKey={stepKey()}
                  icon={toolProcessIconPath(it().kind)}
                  headline={compactToolHeadline(it().kind, it().title, t)}
                  flag={
                    it().status === "failed"
                      ? "failed"
                      : it().status === "pending" || it().status === "in_progress"
                        ? "unfinished"
                        : null
                  }
                  expandable={it().output.length > 0 || it().children.length > 0}
                  expanded={expanded()}
                >
                  <View style={{ width: "100%", minWidth: 0, display: "flex", flexDirection: "column", gap: 8 }}>
                    <If when={it().output.length > 0}>
                      <ToolOutputPartsView
                        parts={it().output}
                        cwd={props.cwd}
                        machine={props.machine}
                      />
                    </If>
                    <If when={it().children.length > 0}>
                      <TranscriptList
                        entries={it().children}
                        path={`${stepKey()}/`}
                        finalReply={null}
                        detail={props.detail}
                        cwd={props.cwd}
                        machine={props.machine}
                      />
                    </If>
                  </View>
                </TranscriptStep>
              </If>

              {/* TurnFailed */}
              <If when={it().type === "turn_failed"}>
                {(() => {
                  const full = () => turnFailedText(it().detail, it().hint, t);
                  const split = () => {
                    const text = full();
                    const nl = text.indexOf("\n");
                    if (nl < 0) return { headline: text, rest: "" };
                    return { headline: text.slice(0, nl), rest: text.slice(nl + 1).trim() };
                  };
                  return (
                    <TranscriptStep
                      stepKey={stepKey()}
                      icon="icons/warning.svg"
                      headline={split().headline}
                      flag="failed"
                      expandable={split().rest.length > 0}
                      expanded={expanded()}
                    >
                      <ToolOutputPartsView
                        parts={[{ Text: split().rest }]}
                        cwd={props.cwd}
                        machine={props.machine}
                      />
                    </TranscriptStep>
                  );
                })()}
              </If>

              {/* Divider / NewSession */}
              <If when={it().type === "divider" || it().type === "new_session"}>
                {(() => {
                  const label = () =>
                    it().type === "divider"
                      ? it().label.trim()
                      : newSessionLabel(it().atMs, t).trim();
                  return (
                    <View
                      style={{
                        paddingTop: 8,
                        paddingBottom: 8,
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        gap: 8,
                      }}
                    >
                      <View style={{ flex: 1, height: 1, backgroundColor: look().palette.hair }} />
                      <If when={label().length > 0}>
                        <Text
                          style={{
                            fontSize: look().type.text_xsmall.size,
                            color: look().palette.faint,
                          }}
                        >
                          {label()}
                        </Text>
                        <View style={{ flex: 1, height: 1, backgroundColor: look().palette.hair }} />
                      </If>
                    </View>
                  );
                })()}
              </If>

              {/* TaskNote */}
              <If when={it().type === "task_note"}>
                {(() => {
                  const tail = () => String(it().note?.outputTail ?? "").trimEnd();
                  return (
                    <TranscriptStep
                      stepKey={stepKey()}
                      icon="icons/info.svg"
                      headline={taskNoteSummary(it().note, t)}
                      expandable={tail().length > 0}
                      expanded={expanded()}
                    >
                      <ToolOutputPartsView
                        parts={[{ Text: tail() }]}
                        cwd={props.cwd}
                        machine={props.machine}
                      />
                    </TranscriptStep>
                  );
                })()}
              </If>
            </View>
          );
        }}
      </Each>
    </View>
  );
}

function RunErrorCard(props: { error: string }) {
  const look = currentTheme;
  return (
    <View
      style={{
        width: "100%",
        paddingLeft: 12,
        paddingRight: 12,
        paddingTop: 10,
        paddingBottom: 10,
        borderRadius: look().radius.row,
        borderWidth: 1,
        borderColor: look().palette.bad,
        backgroundColor: look().palette.surface,
        display: "flex",
        flexDirection: "row",
        alignItems: "flex-start",
        gap: 8,
      }}
    >
      <View style={{ paddingTop: 2, flexShrink: 0 }}>
        <SmeltIcon path="icons/circle-x.svg" size={14} color={look().palette.bad} />
      </View>
      <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 4 }}>
        <Text
          style={{
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.bad,
          }}
        >
          {t("notify.run_failed_body")}
        </Text>
        <Text
          style={{
            fontSize: look().type.ui.size,
            lineHeight: look().type.ui.line_height,
            color: look().palette.text,
          }}
        >
          {props.error}
        </Text>
      </View>
    </View>
  );
}

function ArchivedRunDetailBody(props: { machine: string | null; run: any }) {
  const look = currentTheme;
  const r = () => props.run;

  const expectedKey = computed(() => makeRunDetailKey(props.machine, r()));
  const detail = computed(() => {
    const d = runDetail();
    return d && d.key.runId === r().id ? d : null;
  });
  const fetchError = computed(() => {
    const err = runDetailError();
    return err && sameRunDetailKey(err.key, expectedKey()) ? err.message : null;
  });
  const isLoading = computed(() => {
    const loadingKey = runDetailLoading();
    return Boolean(loadingKey && sameRunDetailKey(loadingKey, expectedKey()));
  });

  const summary = computed(() => {
    return runSummaryInfo(r(), t);
  });
  const errText = computed(() => {
    return runErrorText(r(), t);
  });
  const metadata = computed(() => {
    return runMetadataRows(r(), t);
  });
  const finalReply = computed(() => {
    const d = detail();
    return d ? finalReplyIndex(d.output, d.transcript) : null;
  });
  const processMeta = computed(() => {
    const d = detail();
    return d ? transcriptSummary(d.transcript, t) : "";
  });

  const inputMarkdown = computed(() => {
    const runObj = r();
    if (isShellAction(runObj?.context?.action)) {
      const cmd = formatShellCommand(runObj.context.action).trim();
      return cmd ? fencedBlock("sh", cmd, t) : t("run.none");
    }
    const prompt = runObj?.context?.prompt;
    return typeof prompt === "string" && prompt.trim().length > 0 ? prompt : t("run.none");
  });

  const cwd = () => (typeof r()?.context?.cwd === "string" ? r().context.cwd : null);

  return (
    <View style={{ width: "100%", display: "flex", flexDirection: "column", gap: 20 }}>
      {/* Bug Fix #5: Shell 运行显示终端图标 + Shell，Agent 运行显示 Bot + 智能体名 */}
      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
        <SmeltIcon path={summary().icon} size={13} color={look().palette.muted} />
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.muted,
          }}
        >
          {summary().actor}
        </Text>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>·</Text>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.muted,
          }}
        >
          {summary().detail}
        </Text>
      </View>

      <If when={errText() != null}>
        <RunErrorCard error={errText() ?? ""} />
      </If>

      {/* Bug Fix #8: `automation.run_detail` RPC 失败时显式展示错误并提供重试按钮 */}
      <If when={fetchError() != null}>
        <ErrorAlert
          testID="automation-run-detail-error"
          message={t("run.load_failed", { error: fetchError() ?? "" })}
          onRetry={retryRunDetail}
        />
      </If>

      <If when={isLoading() && detail() == null}>
        <Text style={{ fontSize: look().type.ui.size, color: look().palette.faint }}>
          {t("run.loading")}
        </Text>
      </If>

      <If when={detail() != null}>
        {/* 运行结果 */}
        <View style={{ width: "100%", display: "flex", flexDirection: "column", gap: 8 }}>
          <Text
            style={{
              fontSize: look().type.ui_medium.size,
              lineHeight: look().type.ui_medium.line_height,
              fontWeight: look().type.ui_medium.weight,
              color: look().palette.text,
            }}
          >
            {t("run.result")}
          </Text>
          <If
            when={detail()?.output != null}
            else={
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.faint }}>
                {isRunStatusActive(r().status)
                  ? t("run.result_pending")
                  : r().error_code != null || r().error != null
                    ? t("run.result_none")
                    : t("run.result_no_reply")}
              </Text>
            }
          >
            <ContentCard style={{ padding: 14 }}>
              <SmeltMarkdown
                markdown={detail()?.output ?? ""}
                cwd={cwd()}
                machine={props.machine}
              />
            </ContentCard>
          </If>
        </View>

        {/* 执行过程 */}
        <If when={(detail()?.transcript?.length ?? 0) > 0}>
          <DetailDisclosure
            blockKey={BLOCK_PROCESS}
            title={t("run.process")}
            meta={processMeta()}
            expanded={Boolean(detail()?.expanded.has(BLOCK_PROCESS))}
          >
            <TranscriptList
              entries={detail()!.transcript}
              path="t/"
              finalReply={finalReply()}
              detail={detail()!}
              cwd={cwd()}
              machine={props.machine}
            />
          </DetailDisclosure>
        </If>

        {/* 本次输入 */}
        <DetailDisclosure
          blockKey={BLOCK_INPUT}
          title={t("run.input")}
          expanded={Boolean(detail()?.expanded.has(BLOCK_INPUT))}
        >
          <ContentCard style={{ padding: 14 }}>
            <SmeltMarkdown
              markdown={inputMarkdown()}
              cwd={cwd()}
              machine={props.machine}
            />
          </ContentCard>
        </DetailDisclosure>

        {/* 智能体指令快照 */}
        <If when={detail()?.instructions != null}>
          <DetailDisclosure
            blockKey={BLOCK_INSTRUCTIONS}
            title={t("run.instructions")}
            expanded={Boolean(detail()?.expanded.has(BLOCK_INSTRUCTIONS))}
          >
            <ContentCard style={{ padding: 14 }}>
              <SmeltMarkdown
                markdown={detail()?.instructions ?? ""}
                cwd={cwd()}
                machine={props.machine}
              />
            </ContentCard>
          </DetailDisclosure>
        </If>

        {/* 运行信息 */}
        <DetailDisclosure
          blockKey={BLOCK_META}
          title={t("run.info")}
          expanded={Boolean(detail()?.expanded.has(BLOCK_META))}
        >
          <ContentCard style={{ paddingLeft: 14, paddingRight: 14, paddingTop: 8, paddingBottom: 8 }}>
            <Each of={metadata()} key={(row: any) => row.label}>
              {(row: () => { label: string; value: string }) => (
                <View
                  style={{
                    paddingTop: 4,
                    paddingBottom: 4,
                    display: "flex",
                    flexDirection: "row",
                    gap: 16,
                  }}
                >
                  <View style={{ width: 112, flexShrink: 0 }}>
                    <Text
                      style={{
                        fontSize: look().type.text_xsmall.size,
                        lineHeight: look().type.text_xsmall.line_height,
                        color: look().palette.faint,
                      }}
                    >
                      {row().label}
                    </Text>
                  </View>
                  <Text
                    style={{
                      flex: 1,
                      minWidth: 0,
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      color: look().palette.muted,
                    }}
                  >
                    {row().value}
                  </Text>
                </View>
              )}
            </Each>
          </ContentCard>
        </DetailDisclosure>
      </If>
    </View>
  );
}

export function RunPane(props: { machine: string | null; run: any }) {
  const look = currentTheme;
  const r = () => props.run;
  const canCancel = () => isRunStatusActive(r().status);

  const liveStatus = stableMemo(() => {
    return liveRunStatusFor(props.machine, r());
  });
  const liveKind = computed(() => liveStatus().kind);
  const liveMachine = computed(() => {
    const s = liveStatus();
    return s.kind === "live" ? ((s.args as any).machine ?? null) : null;
  });
  const liveSessionId = computed(() => {
    const s = liveStatus();
    return s.kind === "live" ? String((s.args as any).session_id ?? "") : "";
  });
  const liveSpec = stableMemo(() => {
    const s = liveStatus();
    return s.kind === "live" ? ((s.args as any).spec ?? null) : null;
  });
  const livePlaceholder = computed(() => {
    const s = liveStatus();
    return s.kind === "live" ? ((s.args as any).placeholder ?? null) : null;
  });
  const liveReuseKey = computed(() => {
    const s = liveStatus();
    return s.kind === "live" ? s.reuseKey : undefined;
  });

  return (
    <View
      style={{
        flex: 1,
        minWidth: 0,
        minHeight: 0,
        display: "flex",
        flexDirection: "column",
      }}
    >
      {/* 顶部运行状态行 */}
      <View
        style={{
          flexShrink: 0,
          paddingLeft: 16,
          paddingRight: 16,
          paddingTop: 12,
          paddingBottom: 12,
          borderBottomWidth: 1,
          borderBottomColor: look().palette.hair,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 8,
        }}
      >
        <StatusDot size={7} color={runStatusColor(r().status, look().palette)} />
        <Text
          style={{
            flex: 1,
            minWidth: 0,
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.text,
            numberOfLines: 1,
          }}
        >
          {`${runSourceLabel(r().source, t)} · ${runStatusLabel(r().status, t)}`}
        </Text>
        <If when={canCancel()}>
          <SmeltButton
            testID="automation-run-pane-cancel"
            variant="danger-quiet"
            size="sm"
            icon="icons/close.svg"
            label={t("menu.stop")}
            onPress={() => cancelAutomationRun(props.machine, r().id)}
          />
        </If>
      </View>

      {/* Bug Fix #2: 在运行详情页里也显式展示 pageError */}
      <If when={pageError() != null}>
        <View style={{ paddingLeft: 16, paddingRight: 16, paddingTop: 12, flexShrink: 0 }}>
          <ErrorAlert
            testID="automation-run-page-error"
            message={pageError() ?? ""}
            onDismiss={() => pageError.set(null)}
          />
        </View>
      </If>

      <If when={liveKind() === "unknown_engine"}>
        <View style={{ paddingLeft: 16, paddingRight: 16, paddingTop: 12, flexShrink: 0 }}>
          <ErrorAlert message={t("error.unknown_engine")} />
        </View>
      </If>

      <If
        when={liveKind() === "live"}
        else={
          <Scroll style={{ flex: 1, minHeight: 0, width: "100%" }}>
            <View
              style={{
                width: "100%",
                maxWidth: 760,
                marginLeft: "auto",
                marginRight: "auto",
                paddingLeft: 24,
                paddingRight: 24,
                paddingTop: 20,
                paddingBottom: 28,
              }}
            >
              <ArchivedRunDetailBody machine={props.machine} run={r()} />
            </View>
          </Scroll>
        }
      >
        <SmeltConversation
          machine={liveMachine()}
          sessionId={liveSessionId()}
          spec={liveSpec()}
          placeholder={livePlaceholder()}
          embedded={true}
          reuseKey={liveReuseKey()}
          style={{ flex: 1, width: "100%", height: "100%", minWidth: 0, minHeight: 0 }}
        />
      </If>
    </View>
  );
}
