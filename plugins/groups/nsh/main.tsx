// 本地智能体群组插件（`smelt.groups`）主视图：
// 左侧群组清单 | 中央共享时间线、审批卡与输入框 | 右侧可折叠成员实时对话（`<SmeltConversation>`）、任务与群设置。

import { computed, Scroll, Text, View } from "nsh";
import { Button as BaseButton, Input } from "nsh/base";
import { SmeltConversation } from "smelt/conversation";
import {
  DIALOG_SURFACE_CLASS,
  SmeltButton,
  SmeltDialog,
  SmeltIcon,
  SmeltMarkdown,
  SmeltSwitch,
  t,
  viewShown,
} from "smelt/sdk";
import { MAIN_VIEW } from "../contract.js";
import { activeMentionPrefix, formatShortTime, pendingApprovalsInMessages } from "../model.js";
import {
  activeGroup,
  addMemberAgent,
  addMemberAlias,
  addMemberBusy,
  addMemberDialogOpen,
  addMemberError,
  addMemberOwnership,
  addMemberRole,
  addMemberRolePrompt,
  addMemberSessionId,
  approvePendingMention,
  archiveActiveGroup,
  availableAgents,
  bootGroups,
  clearError,
  composerKind,
  composerText,
  confirmDeleteActiveGroup,
  createBusy,
  createDescription,
  createDialogOpen,
  createError,
  createMode,
  createTitle,
  currentTheme,
  deleteBusy,
  deleteDialogOpen,
  drawerOpen,
  drawerTab,
  getLinkableSessions,
  type GroupMember,
  type GroupMessage,
  type GroupSummary,
  groups,
  hasMoreMessages,
  insertMention,
  inspectedMemberId,
  loadEarlierMessages,
  loadingMessages,
  messages,
  openAddMemberDialog,
  openCreateDialog,
  pageError,
  rejectPendingMention,
  removeMember,
  replyToSeq,
  selectGroup,
  selectedGroupId,
  sendGroupMessage,
  sending,
  setTaskStatus,
  submitAddMember,
  submitCreateGroup,
  toggleGroupMode,
  toggleMuteMember,
  togglePauseGroup,
  togglePinMessage,
  updateGroupSetting,
  wakeMember,
} from "./state.ts";

bootGroups();

function TextInputField(props: {
  value: string;
  placeholder?: string;
  multiline?: boolean;
  onChange: (value: string) => void;
  onSubmit?: () => void;
  style?: any;
}) {
  const look = currentTheme;
  return (
    <Input
      value={props.value}
      placeholder={props.placeholder}
      multiline={props.multiline}
      onChange={({ value }: { value: string }) => props.onChange(value)}
      onSubmit={props.onSubmit}
      class="rounded-(--radius) border border-input bg-card px-2 text-foreground"
      style={{
        ...(props.multiline ? { minHeight: 72 } : { height: 28 }),
        fontSize: look().type.ui.size,
        lineHeight: look().type.ui.line_height,
        ...(props.style ?? {}),
      }}
    />
  );
}

function GroupListColumn() {
  const look = currentTheme;
  return (
    <View
      style={{
        width: 240,
        flexShrink: 0,
        height: "100%",
        borderRightWidth: 1,
        borderRightColor: look().palette.hair,
        display: "flex",
        flexDirection: "column",
      }}
    >
      <View
        style={{
          minHeight: 48,
          paddingLeft: 14,
          paddingRight: 10,
          borderBottomWidth: 1,
          borderBottomColor: look().palette.hair,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 8,
          flexShrink: 0,
        }}
      >
        <Text
          style={{
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.text,
          }}
        >
          {t("list.title")}
        </Text>
        <SmeltButton
          quiet={true}
          small={true}
          icon="icons/plus.svg"
          label={t("list.new")}
          onPress={() => openCreateDialog(null)}
        />
      </View>

      <Scroll style={{ flex: 1, minHeight: 0, width: "100%" }}>
        <View style={{ padding: 8, display: "flex", flexDirection: "column", gap: 2 }}>
          <If when={groups().length === 0}>
            <View style={{ padding: 12, display: "flex", flexDirection: "column", gap: 6 }}>
              <Text
                style={{
                  fontSize: look().type.ui.size,
                  lineHeight: look().type.ui.line_height,
                  color: look().palette.muted,
                }}
              >
                {t("list.empty")}
              </Text>
              <Text
                style={{
                  fontSize: look().type.text_xsmall.size,
                  lineHeight: look().type.text_xsmall.line_height,
                  color: look().palette.faint,
                }}
              >
                {t("list.empty_hint")}
              </Text>
            </View>
          </If>
          <Each of={groups()} key={(g: GroupSummary) => g.id}>
            {(g: () => GroupSummary) => {
              const selected = computed(() => selectedGroupId() === g().id);
              return (
                <BaseButton
                  selected={selected()}
                  onPress={() => void selectGroup(g().id)}
                  style={{
                    width: "100%",
                    paddingLeft: 10,
                    paddingRight: 10,
                    paddingTop: 8,
                    paddingBottom: 8,
                    borderRadius: look().radius.row,
                    backgroundColor: selected() ? look().palette.row_selected : "transparent",
                    cursor: "default",
                    display: "flex",
                    flexDirection: "column",
                    alignItems: "stretch",
                    gap: 3,
                  }}
                  hoverStyle={{
                    backgroundColor: selected()
                      ? look().palette.row_selected
                      : look().palette.row_hover,
                  }}
                  activeStyle={{ backgroundColor: look().palette.row_selected }}
                >
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      justifyContent: "space-between",
                      gap: 6,
                    }}
                  >
                    <Text
                      style={{
                        flex: 1,
                        minWidth: 0,
                        fontSize: look().type.ui.size,
                        lineHeight: look().type.ui.line_height,
                        fontWeight: selected()
                          ? look().type.ui_medium.weight
                          : look().type.ui.weight,
                        color: look().palette.text,
                        numberOfLines: 1,
                      }}
                    >
                      {g().title}
                    </Text>
                    <If when={g().pending_approval_count > 0}>
                      <View
                        style={{
                          width: 7,
                          height: 7,
                          borderRadius: 4,
                          backgroundColor: look().palette.need,
                          flexShrink: 0,
                        }}
                      />
                    </If>
                  </View>
                  <Text
                    style={{
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      color: selected() ? look().palette.muted_selected : look().palette.faint,
                      numberOfLines: 1,
                    }}
                  >
                    {g().paused
                      ? `${t("list.paused")} · ${t("list.members_count", { count: g().active_member_count })}`
                      : t("list.members_count", { count: g().active_member_count })}
                  </Text>
                </BaseButton>
              );
            }}
          </Each>
        </View>
      </Scroll>
    </View>
  );
}

function GroupHeader() {
  const look = currentTheme;
  const g = activeGroup;
  return (
    <View
      style={{
        minHeight: 48,
        paddingLeft: 16,
        paddingRight: 14,
        paddingTop: 8,
        paddingBottom: 8,
        borderBottomWidth: 1,
        borderBottomColor: look().palette.hair,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        justifyContent: "space-between",
        gap: 10,
        flexShrink: 0,
      }}
    >
      <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
        <Text
          style={{
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.text,
            numberOfLines: 1,
          }}
        >
          {g()?.title ?? ""}
        </Text>
        <If when={Boolean(g()?.description)}>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.faint,
              numberOfLines: 1,
            }}
          >
            {g()?.description ?? ""}
          </Text>
        </If>
      </View>

      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 6,
          flexShrink: 0,
        }}
      >
        <SmeltButton
          quiet={true}
          small={true}
          label={
            g()?.mode === "coordinator" ? t("header.mode_coordinator") : t("header.mode_mention")
          }
          onPress={() => void toggleGroupMode()}
        />
        <SmeltButton
          quiet={true}
          small={true}
          icon="icons/plus.svg"
          label={t("header.add_member")}
          onPress={() => openAddMemberDialog(null)}
        />
        <SmeltButton
          quiet={true}
          small={true}
          icon={g()?.paused ? "icons/play.svg" : "icons/pause.svg"}
          label={g()?.paused ? t("header.resume") : t("header.pause")}
          onPress={() => void togglePauseGroup()}
        />
        <SmeltButton
          quiet={true}
          small={true}
          icon={drawerOpen() ? "icons/panel-right-filled.svg" : "icons/panel-right.svg"}
          label={drawerOpen() ? t("header.drawer_hide") : t("header.drawer_show")}
          onPress={() => drawerOpen.set(!drawerOpen())}
        />
      </View>
    </View>
  );
}

function MemberStrip() {
  const look = currentTheme;
  const members = computed(() => activeGroup()?.members ?? []);
  return (
    <If when={members().length > 0}>
      <View
        style={{
          paddingLeft: 16,
          paddingRight: 16,
          paddingTop: 6,
          paddingBottom: 6,
          borderBottomWidth: 1,
          borderBottomColor: look().palette.hair,
          display: "flex",
          flexDirection: "row",
          flexWrap: "wrap",
          gap: 6,
          flexShrink: 0,
        }}
      >
        <Each of={members()} key={(m: GroupMember) => m.id}>
          {(m: () => GroupMember) => {
            const active = computed(() => inspectedMemberId() === m().id && drawerOpen());
            return (
              <BaseButton
                selected={active()}
                onPress={() => {
                  inspectedMemberId.set(m().id);
                  drawerTab.set("member");
                  drawerOpen.set(true);
                }}
                style={{
                  paddingLeft: 8,
                  paddingRight: 8,
                  height: 24,
                  borderRadius: 12,
                  backgroundColor: active() ? look().palette.row_selected : look().palette.soft,
                  cursor: "default",
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 6,
                }}
                hoverStyle={{
                  backgroundColor: active()
                    ? look().palette.row_selected
                    : look().palette.row_hover,
                }}
              >
                <View
                  style={{
                    width: 6,
                    height: 6,
                    borderRadius: 3,
                    backgroundColor: m().session_alive ? look().palette.ok : look().palette.faint,
                  }}
                />
                <Text
                  style={{
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    color: look().palette.text,
                  }}
                >
                  {`@${m().alias}`}
                </Text>
                <Text
                  style={{
                    fontSize: look().type.small.size,
                    lineHeight: look().type.small.line_height,
                    color: look().palette.faint,
                  }}
                >
                  {t(`role.${m().role_kind}`)}
                </Text>
              </BaseButton>
            );
          }}
        </Each>
      </View>
    </If>
  );
}

function ApprovalBanner() {
  const look = currentTheme;
  const pending = computed(() => pendingApprovalsInMessages(messages()));
  return (
    <If when={pending().length > 0}>
      <View
        style={{
          paddingLeft: 16,
          paddingRight: 16,
          paddingTop: 10,
          paddingBottom: 10,
          borderBottomWidth: 1,
          borderBottomColor: look().palette.hair,
          display: "flex",
          flexDirection: "column",
          gap: 8,
          flexShrink: 0,
        }}
      >
        <Each
          of={pending()}
          key={(item: any) => `${item.message.id}:${item.mention.target_member_id}`}
        >
          {(item: () => any) => (
            <View
              style={{
                padding: 10,
                borderRadius: look().radius.row,
                borderWidth: 1,
                borderColor: look().palette.hair,
                backgroundColor: look().palette.surface,
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 10,
              }}
            >
              <View
                style={{
                  flex: 1,
                  minWidth: 0,
                  display: "flex",
                  flexDirection: "column",
                  gap: 2,
                }}
              >
                <Text
                  style={{
                    fontSize: look().type.ui_medium.size,
                    lineHeight: look().type.ui_medium.line_height,
                    fontWeight: look().type.ui_medium.weight,
                    color: look().palette.need,
                  }}
                >
                  {t("approval.title", { alias: item().mention.target_alias })}
                </Text>
                <Text
                  style={{
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    color: look().palette.muted,
                  }}
                >
                  {t("approval.detail")}
                </Text>
              </View>
              <View style={{ display: "flex", flexDirection: "row", gap: 6, flexShrink: 0 }}>
                <SmeltButton
                  small={true}
                  label={t("approval.reject")}
                  onPress={() =>
                    void rejectPendingMention(
                      item().message.id,
                      item().mention.target_member_id,
                    )
                  }
                />
                <SmeltButton
                  primary={true}
                  small={true}
                  label={t("approval.approve")}
                  onPress={() =>
                    void approvePendingMention(
                      item().message.id,
                      item().mention.target_member_id,
                    )
                  }
                />
              </View>
            </View>
          )}
        </Each>
      </View>
    </If>
  );
}

function MessageRow(props: { msg: () => GroupMessage }) {
  const look = currentTheme;
  const m = props.msg;
  const senderLabel = computed(() => {
    const cur = m();
    if (cur.sender_type === "user") return t("timeline.user_sender");
    if (cur.sender_type === "system") return t("timeline.system_sender");
    return cur.sender_alias ? `@${cur.sender_alias}` : cur.sender_display_name;
  });

  return (
    <View
      style={{
        paddingLeft: 14,
        paddingRight: 14,
        paddingTop: 10,
        paddingBottom: 10,
        borderRadius: look().radius.row,
        backgroundColor: m().sender_type === "user" ? look().palette.soft : "transparent",
        display: "flex",
        flexDirection: "column",
        gap: 6,
      }}
    >
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 8,
        }}
      >
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
          <Text
            style={{
              fontSize: look().type.ui_medium.size,
              lineHeight: look().type.ui_medium.line_height,
              fontWeight: look().type.ui_medium.weight,
              color: look().palette.text,
            }}
          >
            {senderLabel()}
          </Text>
          <Text
            style={{
              fontSize: look().type.small.size,
              lineHeight: look().type.small.line_height,
              color: look().palette.faint,
            }}
          >
            {`#${m().seq} · ${formatShortTime(m().created_at_ms)}`}
          </Text>
          <If when={m().kind === "task" || m().kind === "decision"}>
            <Text
              style={{
                fontSize: look().type.small_medium.size,
                lineHeight: look().type.small_medium.line_height,
                color: m().kind === "decision" ? look().palette.ok : look().palette.accent,
              }}
            >
              {m().kind === "task"
                ? `${t("timeline.kind_task")} · ${t(`task_status.${m().task_status ?? "open"}`)}`
                : t("timeline.kind_decision")}
            </Text>
          </If>
          <If when={m().auto_bridged}>
            <Text
              style={{
                fontSize: look().type.small.size,
                lineHeight: look().type.small.line_height,
                color: look().palette.faint,
              }}
            >
              {t("timeline.auto_bridged")}
            </Text>
          </If>
          <If when={m().pinned}>
            <Text
              style={{
                fontSize: look().type.small.size,
                lineHeight: look().type.small.line_height,
                color: look().palette.muted,
              }}
            >
              {t("timeline.pinned")}
            </Text>
          </If>
        </View>
        <View style={{ display: "flex", flexDirection: "row", gap: 4 }}>
          <SmeltButton
            quiet={true}
            small={true}
            label={t("timeline.reply")}
            onPress={() => replyToSeq.set(m().seq)}
          />
          <SmeltButton
            quiet={true}
            small={true}
            icon={m().pinned ? "icons/pin-off.svg" : "icons/pin.svg"}
            label={m().pinned ? t("timeline.unpin") : t("timeline.pin")}
            onPress={() => void togglePinMessage(m().seq, !m().pinned)}
          />
        </View>
      </View>

      <If when={m().reply_to_seq != null}>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.faint,
          }}
        >
          {t("timeline.reply_to", { seq: m().reply_to_seq })}
        </Text>
      </If>

      <SmeltMarkdown
        markdown={m().content}
        cwd={activeGroup()?.project_root ?? null}
        machine={null}
      />

      <If when={m().mentions.length > 0}>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            flexWrap: "wrap",
            gap: 6,
            paddingTop: 2,
          }}
        >
          <Each
            of={m().mentions}
            key={(mn: any) => `${mn.message_id}:${mn.target_member_id}`}
          >
            {(mn: () => any) => {
              const color = computed(() => {
                const status = mn().delivery_status;
                if (status === "delivered") return look().palette.ok;
                if (status === "pending_approval") return look().palette.need;
                if (status === "failed" || status === "rejected") return look().palette.bad;
                return look().palette.faint;
              });
              return (
                <Text
                  style={{
                    fontSize: look().type.small.size,
                    lineHeight: look().type.small.line_height,
                    color: color(),
                  }}
                >
                  {`@${mn().target_alias} · ${t(`mention_status.${mn().delivery_status}`)}`}
                </Text>
              );
            }}
          </Each>
        </View>
      </If>
    </View>
  );
}

function TimelineComposer() {
  const look = currentTheme;
  const mentionSuggestions = computed(() => {
    const prefix = activeMentionPrefix(composerText());
    if (prefix == null) return [];
    const list = activeGroup()?.members ?? [];
    return list.filter((m) => m.alias.startsWith(prefix)).slice(0, 6);
  });

  return (
    <View
      style={{
        paddingLeft: 16,
        paddingRight: 16,
        paddingTop: 10,
        paddingBottom: 14,
        borderTopWidth: 1,
        borderTopColor: look().palette.hair,
        display: "flex",
        flexDirection: "column",
        gap: 8,
        flexShrink: 0,
      }}
    >
      <If when={replyToSeq() != null}>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            justifyContent: "space-between",
          }}
        >
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
            }}
          >
            {t("timeline.reply_to", { seq: replyToSeq() })}
          </Text>
          <SmeltButton
            quiet={true}
            small={true}
            label={t("timeline.cancel_reply")}
            onPress={() => replyToSeq.set(null)}
          />
        </View>
      </If>

      <If when={mentionSuggestions().length > 0}>
        <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
          <Each of={mentionSuggestions()} key={(m: GroupMember) => m.id}>
            {(m: () => GroupMember) => (
              <SmeltButton
                small={true}
                label={`@${m().alias}`}
                onPress={() => insertMention(m().alias)}
              />
            )}
          </Each>
        </View>
      </If>

      <TextInputField
        value={composerText()}
        placeholder={t("composer.placeholder")}
        multiline={true}
        onChange={(val) => composerText.set(val)}
        onSubmit={() => void sendGroupMessage()}
      />

      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 8,
        }}
      >
        <View style={{ display: "flex", flexDirection: "row", gap: 4 }}>
          <SmeltButton
            quiet={composerKind() !== "chat"}
            small={true}
            label={t("composer.kind_chat")}
            onPress={() => composerKind.set("chat")}
          />
          <SmeltButton
            quiet={composerKind() !== "task"}
            small={true}
            label={t("composer.kind_task")}
            onPress={() => composerKind.set("task")}
          />
          <SmeltButton
            quiet={composerKind() !== "decision"}
            small={true}
            label={t("composer.kind_decision")}
            onPress={() => composerKind.set("decision")}
          />
        </View>
        <SmeltButton
          primary={true}
          small={true}
          icon="icons/arrow-up.svg"
          label={t("composer.send")}
          disabled={sending() || composerText().trim().length === 0}
          onPress={() => void sendGroupMessage()}
        />
      </View>
    </View>
  );
}

function InspectorDrawer() {
  const look = currentTheme;
  const g = activeGroup;
  const inspected = computed(() => {
    const id = inspectedMemberId();
    return (g()?.members ?? []).find((m) => m.id === id) ?? null;
  });

  return (
    <If when={drawerOpen() && g() != null}>
      <View
        style={{
          width: 380,
          flexShrink: 0,
          height: "100%",
          borderLeftWidth: 1,
          borderLeftColor: look().palette.hair,
          display: "flex",
          flexDirection: "column",
        }}
      >
        <View
          style={{
            minHeight: 44,
            paddingLeft: 12,
            paddingRight: 12,
            borderBottomWidth: 1,
            borderBottomColor: look().palette.hair,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 6,
            flexShrink: 0,
          }}
        >
          <SmeltButton
            quiet={drawerTab() !== "member"}
            small={true}
            label={t("drawer.tab_conversation")}
            onPress={() => drawerTab.set("member")}
          />
          <SmeltButton
            quiet={drawerTab() !== "tasks"}
            small={true}
            label={t("drawer.tab_tasks")}
            onPress={() => drawerTab.set("tasks")}
          />
          <SmeltButton
            quiet={drawerTab() !== "settings"}
            small={true}
            label={t("drawer.tab_settings")}
            onPress={() => drawerTab.set("settings")}
          />
        </View>

        <If when={drawerTab() === "member"}>
          <If
            when={inspected() != null}
            else={
              <View style={{ padding: 16 }}>
                <Text
                  style={{
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    color: look().palette.faint,
                  }}
                >
                  {t("drawer.select_member")}
                </Text>
              </View>
            }
          >
            <View style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column" }}>
              <View
                style={{
                  padding: 10,
                  borderBottomWidth: 1,
                  borderBottomColor: look().palette.hair,
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 6,
                  flexShrink: 0,
                }}
              >
                <View
                  style={{
                    display: "flex",
                    flexDirection: "column",
                    gap: 2,
                    minWidth: 0,
                    flex: 1,
                  }}
                >
                  <Text
                    style={{
                      fontSize: look().type.ui_medium.size,
                      lineHeight: look().type.ui_medium.line_height,
                      fontWeight: look().type.ui_medium.weight,
                      color: look().palette.text,
                      numberOfLines: 1,
                    }}
                  >
                    {`@${inspected()!.alias}`}
                  </Text>
                  <Text
                    style={{
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      color: look().palette.faint,
                    }}
                  >
                    {inspected()!.ownership === "group_owned"
                      ? t("drawer.ownership_owned")
                      : t("drawer.ownership_linked")}
                  </Text>
                </View>
                <View style={{ display: "flex", flexDirection: "row", gap: 4 }}>
                  <SmeltButton
                    quiet={true}
                    small={true}
                    label={t("drawer.wake")}
                    onPress={() => void wakeMember(inspected()!.id)}
                  />
                  <SmeltButton
                    quiet={true}
                    small={true}
                    label={inspected()!.muted ? t("drawer.unmute") : t("drawer.mute")}
                    onPress={() => void toggleMuteMember(inspected()!)}
                  />
                  <SmeltButton
                    quiet={true}
                    danger={true}
                    small={true}
                    label={t("drawer.remove")}
                    onPress={() => void removeMember(inspected()!)}
                  />
                </View>
              </View>
              <SmeltConversation
                machine={null}
                sessionId={inspected()!.session_id}
                embedded={true}
                active={viewShown(MAIN_VIEW) && drawerOpen() && drawerTab() === "member"}
                reuseKey={`group-member:${inspected()!.session_id}`}
                style={{ flex: 1, minHeight: 0, width: "100%" }}
              />
            </View>
          </If>
        </If>

        <If when={drawerTab() === "tasks"}>
          <Scroll style={{ flex: 1, minHeight: 0, width: "100%" }}>
            <View style={{ padding: 12, display: "flex", flexDirection: "column", gap: 8 }}>
              <If when={(g()?.open_tasks ?? []).length === 0}>
                <Text
                  style={{
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    color: look().palette.faint,
                  }}
                >
                  {t("drawer.no_tasks")}
                </Text>
              </If>
              <Each of={g()?.open_tasks ?? []} key={(task: GroupMessage) => task.id}>
                {(task: () => GroupMessage) => (
                  <View
                    style={{
                      padding: 10,
                      borderRadius: look().radius.row,
                      borderWidth: 1,
                      borderColor: look().palette.hair,
                      backgroundColor: look().palette.surface,
                      display: "flex",
                      flexDirection: "column",
                      gap: 6,
                    }}
                  >
                    <Text
                      style={{
                        fontSize: look().type.ui.size,
                        lineHeight: look().type.ui.line_height,
                        color: look().palette.text,
                      }}
                    >
                      {`#${task().seq} ${task().content}`}
                    </Text>
                    <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 4 }}>
                      <SmeltButton
                        quiet={task().task_status !== "open"}
                        small={true}
                        label={t("task_status.open")}
                        onPress={() => void setTaskStatus(task().seq, "open")}
                      />
                      <SmeltButton
                        quiet={task().task_status !== "in_progress"}
                        small={true}
                        label={t("task_status.in_progress")}
                        onPress={() => void setTaskStatus(task().seq, "in_progress")}
                      />
                      <SmeltButton
                        quiet={task().task_status !== "done"}
                        small={true}
                        label={t("task_status.done")}
                        onPress={() => void setTaskStatus(task().seq, "done")}
                      />
                      <SmeltButton
                        quiet={task().task_status !== "blocked"}
                        small={true}
                        label={t("task_status.blocked")}
                        onPress={() => void setTaskStatus(task().seq, "blocked")}
                      />
                    </View>
                  </View>
                )}
              </Each>
            </View>
          </Scroll>
        </If>

        <If when={drawerTab() === "settings"}>
          <Scroll style={{ flex: 1, minHeight: 0, width: "100%" }}>
            <View style={{ padding: 14, display: "flex", flexDirection: "column", gap: 16 }}>
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 10,
                }}
              >
                <View style={{ flex: 1, display: "flex", flexDirection: "column", gap: 2 }}>
                  <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                    {t("drawer.cross_talk_label")}
                  </Text>
                  <Text
                    style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}
                  >
                    {t("drawer.cross_talk_hint")}
                  </Text>
                </View>
                <SmeltSwitch
                  checked={Boolean(g()?.allow_worker_cross_talk)}
                  accessibilityLabel={t("drawer.cross_talk_label")}
                  onChange={(checked: boolean) =>
                    void updateGroupSetting({ allow_worker_cross_talk: checked })
                  }
                />
              </View>

              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 10,
                }}
              >
                <View style={{ flex: 1, display: "flex", flexDirection: "column", gap: 2 }}>
                  <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                    {t("drawer.auto_bridge_label")}
                  </Text>
                  <Text
                    style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}
                  >
                    {t("drawer.auto_bridge_hint")}
                  </Text>
                </View>
                <SmeltSwitch
                  checked={Boolean(g()?.auto_bridge_mentions)}
                  accessibilityLabel={t("drawer.auto_bridge_label")}
                  onChange={(checked: boolean) =>
                    void updateGroupSetting({ auto_bridge_mentions: checked })
                  }
                />
              </View>

              <View
                style={{
                  paddingTop: 12,
                  borderTopWidth: 1,
                  borderTopColor: look().palette.hair,
                  display: "flex",
                  flexDirection: "row",
                  gap: 8,
                }}
              >
                <SmeltButton
                  small={true}
                  icon="icons/archive.svg"
                  label={t("header.archive")}
                  onPress={() => void archiveActiveGroup()}
                />
                <SmeltButton
                  quiet={true}
                  danger={true}
                  small={true}
                  icon="icons/trash.svg"
                  label={t("header.delete")}
                  onPress={() => deleteDialogOpen.set(true)}
                />
              </View>
            </View>
          </Scroll>
        </If>
      </View>
    </If>
  );
}

function CreateGroupDialog() {
  const look = currentTheme;
  return (
    <SmeltDialog
      open={createDialogOpen()}
      accessibilityLabel={t("dialog.create_title")}
      onOpenChange={({ open }: { open: boolean }) => {
        if (!open) createDialogOpen.set(false);
      }}
      class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
      style={{ width: 420, gap: 12 }}
    >
      <Text
        style={{
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
          color: look().palette.text,
        }}
      >
        {t("dialog.create_title")}
      </Text>
      <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
          {t("dialog.group_title_label")}
        </Text>
        <TextInputField
          value={createTitle()}
          placeholder={t("dialog.group_title_placeholder")}
          onChange={(v) => createTitle.set(v)}
          onSubmit={() => void submitCreateGroup()}
        />
      </View>
      <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
          {t("dialog.group_desc_label")}
        </Text>
        <TextInputField
          value={createDescription()}
          placeholder={t("dialog.group_desc_placeholder")}
          onChange={(v) => createDescription.set(v)}
        />
      </View>
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
        }}
      >
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
          {t("dialog.group_mode_label")}
        </Text>
        <View style={{ display: "flex", flexDirection: "row", gap: 6 }}>
          <SmeltButton
            quiet={createMode() !== "mention"}
            small={true}
            label={t("header.mode_mention")}
            onPress={() => createMode.set("mention")}
          />
          <SmeltButton
            quiet={createMode() !== "coordinator"}
            small={true}
            label={t("header.mode_coordinator")}
            onPress={() => createMode.set("coordinator")}
          />
        </View>
      </View>
      <If when={createError() != null}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
          {createError() ?? ""}
        </Text>
      </If>
      <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
        <SmeltButton
          quiet={true}
          small={true}
          label={t("dialog.cancel")}
          onPress={() => createDialogOpen.set(false)}
        />
        <SmeltButton
          primary={true}
          small={true}
          disabled={createBusy()}
          label={t("dialog.create_confirm")}
          onPress={() => void submitCreateGroup()}
        />
      </View>
    </SmeltDialog>
  );
}

function AddMemberDialog() {
  const look = currentTheme;
  const linkable = computed(() => getLinkableSessions().slice(0, 8));
  return (
    <SmeltDialog
      open={addMemberDialogOpen()}
      accessibilityLabel={t("dialog.add_member_title")}
      onOpenChange={({ open }: { open: boolean }) => {
        if (!open) addMemberDialogOpen.set(false);
      }}
      class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
      style={{ width: 440, gap: 12 }}
    >
      <Text
        style={{
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
          color: look().palette.text,
        }}
      >
        {t("dialog.add_member_title")}
      </Text>

      <View style={{ display: "flex", flexDirection: "row", gap: 6 }}>
        <SmeltButton
          quiet={addMemberOwnership() !== "group_owned"}
          small={true}
          label={t("dialog.ownership_new")}
          onPress={() => addMemberOwnership.set("group_owned")}
        />
        <SmeltButton
          quiet={addMemberOwnership() !== "linked"}
          small={true}
          label={t("dialog.ownership_link")}
          onPress={() => addMemberOwnership.set("linked")}
        />
      </View>

      <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
          {t("dialog.alias_label")}
        </Text>
        <TextInputField
          value={addMemberAlias()}
          placeholder={t("dialog.alias_placeholder")}
          onChange={(v) => addMemberAlias.set(v)}
        />
      </View>

      <If when={addMemberOwnership() === "group_owned"}>
        <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
            {t("dialog.agent_label")}
          </Text>
          <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
            <Each of={availableAgents()} key={(a: any) => a.id}>
              {(a: () => any) => (
                <SmeltButton
                  quiet={addMemberAgent() !== a().id}
                  small={true}
                  label={a().label}
                  onPress={() => addMemberAgent.set(a().id)}
                />
              )}
            </Each>
          </View>
        </View>
      </If>

      <If when={addMemberOwnership() === "linked"}>
        <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
            {t("dialog.session_label")}
          </Text>
          <If when={linkable().length === 0}>
            <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
              {t("dialog.no_linkable_sessions")}
            </Text>
          </If>
          <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <Each of={linkable()} key={(s: any) => s.session_id}>
              {(s: () => any) => (
                <SmeltButton
                  quiet={addMemberSessionId() !== s().session_id}
                  small={true}
                  label={s().title}
                  onPress={() => addMemberSessionId.set(s().session_id)}
                />
              )}
            </Each>
          </View>
        </View>
      </If>

      <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
          {t("dialog.role_label")}
        </Text>
        <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
          <SmeltButton
            quiet={addMemberRole() !== "coordinator"}
            small={true}
            label={t("role.coordinator")}
            onPress={() => addMemberRole.set("coordinator")}
          />
          <SmeltButton
            quiet={addMemberRole() !== "worker"}
            small={true}
            label={t("role.worker")}
            onPress={() => addMemberRole.set("worker")}
          />
          <SmeltButton
            quiet={addMemberRole() !== "reviewer"}
            small={true}
            label={t("role.reviewer")}
            onPress={() => addMemberRole.set("reviewer")}
          />
          <SmeltButton
            quiet={addMemberRole() !== "observer"}
            small={true}
            label={t("role.observer")}
            onPress={() => addMemberRole.set("observer")}
          />
        </View>
      </View>

      <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
          {t("dialog.role_prompt_label")}
        </Text>
        <TextInputField
          value={addMemberRolePrompt()}
          placeholder={t("dialog.role_prompt_placeholder")}
          onChange={(v) => addMemberRolePrompt.set(v)}
        />
      </View>

      <If when={addMemberError() != null}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
          {addMemberError() ?? ""}
        </Text>
      </If>

      <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
        <SmeltButton
          quiet={true}
          small={true}
          label={t("dialog.cancel")}
          onPress={() => addMemberDialogOpen.set(false)}
        />
        <SmeltButton
          primary={true}
          small={true}
          disabled={addMemberBusy()}
          label={t("dialog.add_member_confirm")}
          onPress={() => void submitAddMember()}
        />
      </View>
    </SmeltDialog>
  );
}

function DeleteGroupDialog() {
  const look = currentTheme;
  return (
    <SmeltDialog
      open={deleteDialogOpen()}
      role="alertdialog"
      accessibilityLabel={t("dialog.delete_title")}
      onOpenChange={({ open }: { open: boolean }) => {
        if (!open) deleteDialogOpen.set(false);
      }}
      class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
      style={{ width: 420, gap: 12 }}
    >
      <Text
        style={{
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
          color: look().palette.text,
        }}
      >
        {t("dialog.delete_title")}
      </Text>
      <Text
        style={{
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          color: look().palette.muted,
        }}
      >
        {t("dialog.delete_body")}
      </Text>
      <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
        <SmeltButton
          quiet={true}
          small={true}
          label={t("dialog.cancel")}
          onPress={() => deleteDialogOpen.set(false)}
        />
        <SmeltButton
          danger={true}
          small={true}
          disabled={deleteBusy()}
          label={t("dialog.delete_confirm")}
          onPress={() => void confirmDeleteActiveGroup()}
        />
      </View>
    </SmeltDialog>
  );
}

export function main() {
  const look = currentTheme;
  return (
    <View
      style={{
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
        display: "flex",
        flexDirection: "row",
      }}
    >
      <GroupListColumn />

      <View
        style={{
          flex: 1,
          minWidth: 0,
          minHeight: 0,
          display: "flex",
          flexDirection: "column",
        }}
      >
        <If when={pageError() != null}>
          <View
            style={{
              paddingLeft: 16,
              paddingRight: 16,
              paddingTop: 8,
              paddingBottom: 8,
              borderBottomWidth: 1,
              borderBottomColor: look().palette.hair,
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              justifyContent: "space-between",
              gap: 8,
              flexShrink: 0,
            }}
          >
            <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
              {pageError() ?? ""}
            </Text>
            <SmeltButton quiet={true} small={true} icon="icons/close.svg" onPress={clearError} />
          </View>
        </If>

        <If
          when={activeGroup() != null}
          else={
            <View
              style={{
                flex: 1,
                display: "flex",
                flexDirection: "column",
                alignItems: "center",
                justifyContent: "center",
                gap: 10,
                padding: 24,
              }}
            >
              <SmeltIcon path="icons/messages-square.svg" size={24} color={look().palette.faint} />
              <Text style={{ fontSize: look().type.ui_medium.size, color: look().palette.text }}>
                {t("list.empty")}
              </Text>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                {t("list.empty_hint")}
              </Text>
              <SmeltButton
                primary={true}
                small={true}
                icon="icons/plus.svg"
                label={t("list.new")}
                onPress={() => openCreateDialog(null)}
              />
            </View>
          }
        >
          <GroupHeader />
          <MemberStrip />
          <ApprovalBanner />

          <Scroll style={{ flex: 1, minHeight: 0, width: "100%" }}>
            <View
              style={{
                width: "100%",
                maxWidth: 768,
                marginLeft: "auto",
                marginRight: "auto",
                paddingLeft: 20,
                paddingRight: 20,
                paddingTop: 14,
                paddingBottom: 20,
                display: "flex",
                flexDirection: "column",
                gap: 10,
              }}
            >
              <If when={hasMoreMessages()}>
                <View style={{ display: "flex", flexDirection: "row", justifyContent: "center" }}>
                  <SmeltButton
                    quiet={true}
                    small={true}
                    disabled={loadingMessages()}
                    label={t("timeline.load_earlier")}
                    onPress={() => void loadEarlierMessages()}
                  />
                </View>
              </If>
              <If when={messages().length === 0 && !loadingMessages()}>
                <Text
                  style={{
                    fontSize: look().type.ui.size,
                    color: look().palette.faint,
                    paddingTop: 12,
                  }}
                >
                  {t("timeline.empty")}
                </Text>
              </If>
              <Each of={messages()} key={(m: GroupMessage) => m.id}>
                {(m: () => GroupMessage) => <MessageRow msg={m} />}
              </Each>
            </View>
          </Scroll>

          <TimelineComposer />
        </If>
      </View>

      <InspectorDrawer />
      <CreateGroupDialog />
      <AddMemberDialog />
      <DeleteGroupDialog />
    </View>
  );
}

export default main;
