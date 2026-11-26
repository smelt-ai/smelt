// 智能体群组插件（`smelt.groups`）入口：
// - `sidebar`：挂在左栏 `smelt.session-list/main` 上方的「活跃群组」紧凑折叠区；
// - `main`：主区域异构多智能体群聊工作台（群头 + 成员轨道 + 时间线 + 盲评对比卡 + 权限放行卡 + 输入框 @补全 + 右侧可折叠抽屉 `<SmeltConversation>` / 任务 / 分支合并）。
import { computed, Scroll, Text, View } from "nsh";
import { Input } from "nsh/base";
import { SmeltConversation } from "smelt/conversation";
import {
  DIALOG_SURFACE_CLASS,
  INPUT_CLASS,
  SmeltButton,
  SmeltDialog,
  SmeltDisclosure,
  SmeltDisclosureChevron,
  SmeltIcon,
  t,
} from "smelt/sdk";
import {
  activeMentionQuery,
  applyMentionCompletion,
  filterMentionCandidates,
  GROUP_TEMPLATES,
  groupBlindSubmissions,
  sameGroupRef,
} from "../model.js";
import {
  activeDetail,
  activeMessages,
  addMemberDraft,
  allGroups,
  availableConversationAgents,
  availableMachines,
  bootGroups,
  closeAddMemberDialog,
  closeCreateGroupDialog,
  composerText,
  createDraft,
  currentTheme,
  decideMentionApproval,
  detailLoading,
  drawerOpen,
  drawerTab,
  inspectMember,
  inspectedMemberId,
  loadMemberDiff,
  machineDisplayName,
  mergeMemberBranch,
  openAddMemberDialog,
  openCreateGroupDialog,
  openGroup,
  pageError,
  patchAddMemberDraft,
  patchCreateDraft,
  quickWakeFromMessage,
  revealBlindRound,
  selectedGroup,
  sendComposerMessage,
  sending,
  sidebarCollapsed,
  submitAddMember,
  submitCreateGroup,
  toggleGroupPaused,
  toggleMessagePinned,
  worktreeBusyMember,
  worktreeDiffs,
} from "./state.ts";

bootGroups();

function statusDotColor(status: string): string {
  const p = currentTheme().palette;
  if (status === "waiting_approval") return p.need;
  if (status === "running") return p.accent;
  if (status === "offline") return p.bad;
  return p.faint;
}

// ============================================================================
// 左侧边栏：`smelt.groups/sidebar`
// ============================================================================

export function sidebar() {
  const look = currentTheme;
  const groups = computed(() => allGroups());

  return (
    <View
      style={{
        display: "flex",
        flexDirection: "column",
        paddingLeft: 8,
        paddingRight: 8,
        paddingTop: 4,
        paddingBottom: 6,
        borderBottomWidth: 1,
        borderBottomColor: look().palette.hair,
        flexShrink: 0,
      }}
    >
      <View
        style={{
          height: 26,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          paddingLeft: 4,
          paddingRight: 2,
          gap: 6,
        }}
      >
        <View
          onPress={() => sidebarCollapsed.set(!sidebarCollapsed())}
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 4,
            flex: 1,
            minWidth: 0,
          }}
        >
          <SmeltDisclosureChevron open={!sidebarCollapsed()} size={12} color={look().palette.faint} />
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              fontWeight: look().type.small_medium.weight,
              color: look().palette.faint,
            }}
          >
            {t("sidebar.title")} ({groups().length})
          </Text>
        </View>
        <SmeltButton
          quiet
          small
          icon="icons/plus.svg"
          tooltip={t("menu.new_group")}
          onPress={() => openCreateGroupDialog({ hostMachine: null })}
        />
      </View>

      <SmeltDisclosure open={!sidebarCollapsed()}>
        <View style={{ display: "flex", flexDirection: "column", gap: 2, paddingTop: 2 }}>
          <If
            when={groups().length > 0}
            else={
              <Text
                style={{
                  fontSize: look().type.text_xsmall.size,
                  lineHeight: look().type.text_xsmall.line_height,
                  color: look().palette.faint,
                  paddingLeft: 8,
                  paddingVertical: 4,
                }}
              >
                {t("sidebar.empty")}
              </Text>
            }
          >
            <Each of={groups()} key={(item: any) => `${item.machine ?? "local"}::${item.group.id}`}>
              {(item: () => any) => {
                const isSelected = computed(() =>
                  sameGroupRef(selectedGroup(), {
                    machine: item().machine ?? null,
                    id: item().group.id,
                  }),
                );
                const attention = computed(
                  () =>
                    (item().waiting_approval_count ?? 0) +
                    (item().pending_mention_approval_count ?? 0),
                );
                return (
                  <View
                    onPress={() =>
                      void openGroup({ machine: item().machine ?? null, id: item().group.id }, true)
                    }
                    style={{
                      height: 30,
                      paddingLeft: 8,
                      paddingRight: 8,
                      borderRadius: look().radius.row,
                      backgroundColor: isSelected() ? look().palette.selected : "transparent",
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 6,
                    }}
                  >
                    <View
                      style={{
                        width: 7,
                        height: 7,
                        borderRadius: 4,
                        backgroundColor:
                          attention() > 0
                            ? look().palette.need
                            : (item().running_count ?? 0) > 0
                              ? look().palette.accent
                              : look().palette.faint,
                        flexShrink: 0,
                      }}
                    />
                    <Text
                      class="flex-1 min-w-0"
                      style={{
                        fontSize: look().type.ui.size,
                        lineHeight: look().type.ui.line_height,
                        color: look().palette.text,
                        numberOfLines: 1,
                      }}
                    >
                      {item().group.title}
                    </Text>
                    <If when={item().machine != null}>
                      <Text
                        style={{
                          fontSize: look().type.text_xsmall.size,
                          color: look().palette.faint,
                          flexShrink: 0,
                        }}
                      >
                        {machineDisplayName(item().machine)}
                      </Text>
                    </If>
                    <If when={attention() > 0}>
                      <View
                        style={{
                          paddingLeft: 5,
                          paddingRight: 5,
                          height: 16,
                          borderRadius: 8,
                          backgroundColor: look().palette.need,
                          alignItems: "center",
                          justifyContent: "center",
                          flexShrink: 0,
                        }}
                      >
                        <Text
                          style={{
                            fontSize: 10,
                            color: look().palette.surface,
                            fontWeight: "600",
                          }}
                        >
                          {attention()}
                        </Text>
                      </View>
                    </If>
                  </View>
                );
              }}
            </Each>
          </If>
        </View>
      </SmeltDisclosure>
    </View>
  );
}

// ============================================================================
// 主工作台：`smelt.groups/main`
// ============================================================================

function GroupHeader() {
  const look = currentTheme;
  const detail = activeDetail;
  const ref = selectedGroup;

  return (
    <View
      style={{
        minHeight: 48,
        paddingLeft: 18,
        paddingRight: 16,
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
      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8, flex: 1, minWidth: 0 }}>
        <SmeltIcon path="icons/users.svg" size={16} color={look().palette.muted} />
        <Text
          class="min-w-0"
          style={{
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.text,
            numberOfLines: 1,
          }}
        >
          {detail()?.group?.title ?? ""}
        </Text>
        <View
          style={{
            paddingLeft: 6,
            paddingRight: 6,
            paddingTop: 2,
            paddingBottom: 2,
            borderRadius: 6,
            backgroundColor: look().palette.soft,
          }}
        >
          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
            {machineDisplayName(ref()?.machine)}
          </Text>
        </View>
        <View
          style={{
            paddingLeft: 6,
            paddingRight: 6,
            paddingTop: 2,
            paddingBottom: 2,
            borderRadius: 6,
            backgroundColor: look().palette.soft,
          }}
        >
          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
            {detail()?.group?.mode === "coordinator"
              ? t("header.mode_coordinator")
              : t("header.mode_mention")}
          </Text>
        </View>
      </View>

      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 6, flexShrink: 0 }}>
        <SmeltButton
          quiet
          small
          label={t("header.add_member")}
          onPress={() => openAddMemberDialog()}
        />
        <SmeltButton
          quiet
          small
          danger={!detail()?.group?.paused}
          label={detail()?.group?.paused ? t("header.resume") : t("header.pause")}
          onPress={() => void toggleGroupPaused()}
        />
        <SmeltButton
          quiet
          small
          toggled={drawerOpen()}
          label={t("drawer.tab_branches")}
          onPress={() => {
            if (drawerOpen() && drawerTab() === "branches") {
              drawerOpen.set(false);
            } else {
              drawerTab.set("branches");
              drawerOpen.set(true);
            }
          }}
        />
      </View>
    </View>
  );
}

function MemberStatusRail() {
  const look = currentTheme;
  const members = computed(() => activeDetail()?.members ?? []);

  return (
    <View
      style={{
        paddingLeft: 18,
        paddingRight: 16,
        paddingTop: 6,
        paddingBottom: 6,
        borderBottomWidth: 1,
        borderBottomColor: look().palette.hair,
        display: "flex",
        flexDirection: "row",
        flexWrap: "wrap",
        alignItems: "center",
        gap: 6,
        flexShrink: 0,
      }}
    >
      <Each of={members()} key={(m: any) => m.id}>
        {(m: () => any) => {
          const active = computed(() => drawerOpen() && inspectedMemberId() === m().id);
          return (
            <View
              onPress={() => inspectMember(m().id)}
              style={{
                paddingLeft: 8,
                paddingRight: 8,
                height: 26,
                borderRadius: 13,
                borderWidth: 1,
                borderColor: active() ? look().palette.accent : look().palette.hair,
                backgroundColor: active() ? look().palette.selected : look().palette.surface,
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                gap: 6,
              }}
            >
              <View
                style={{
                  width: 7,
                  height: 7,
                  borderRadius: 4,
                  backgroundColor: statusDotColor(m().live_status),
                }}
              />
              <Text style={{ fontSize: look().type.small.size, color: look().palette.text, fontWeight: "500" }}>
                @{m().alias}
              </Text>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                {m().role_title || m().role_kind}
                {m().runtime_id ? ` · ${machineDisplayName(m().runtime_id)}` : ""}
              </Text>
            </View>
          );
        }}
      </Each>
    </View>
  );
}

function MessageTimeline() {
  const look = currentTheme;
  const messages = activeMessages;
  const members = computed(() => activeDetail()?.members ?? []);
  const blindMap = computed(() => groupBlindSubmissions(messages()));

  const memberById = (id: string | null | undefined) =>
    members().find((m: any) => m.id === id) ?? null;

  return (
    <Scroll style={{ flex: 1, minHeight: 0 }}>
      <View
        style={{
          paddingLeft: 18,
          paddingRight: 18,
          paddingTop: 14,
          paddingBottom: 20,
          display: "flex",
          flexDirection: "column",
          gap: 12,
        }}
      >
        <If when={detailLoading() && messages().length === 0}>
          <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
            Loading…
          </Text>
        </If>

        <If when={!detailLoading() && messages().length === 0}>
          <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
            {t("timeline.no_messages")}
          </Text>
        </If>

        <Each of={messages()} key={(msg: any) => msg.id}>
          {(msg: () => any) => {
            // 盲评提交合并展示在对应 blind_prompt 对比卡片下，不重复刷屏
            if (msg().kind === "blind_submission") return <View />;

            const sender = computed(() =>
              msg().sender_type === "member" ? memberById(msg().sender_member_id) : null,
            );
            const senderLabel = computed(() => {
              if (msg().sender_type === "user") return "You";
              if (msg().sender_type === "system") return "System";
              return sender() ? `@${sender().alias}` : "Agent";
            });
            const pendingApprovals = computed(() =>
              (msg().mentions ?? []).filter((mn: any) => mn.delivery_status === "pending_approval"),
            );
            const isUnmentionedUserChat = computed(
              () =>
                msg().sender_type === "user" &&
                msg().kind === "chat" &&
                (msg().mentions ?? []).length === 0 &&
                members().length > 0,
            );
            const blindSubs = computed(() =>
              msg().kind === "blind_prompt" ? (blindMap().get(msg().seq) ?? []) : [],
            );
            const blindRevealed = computed(
              () => msg().blind_revealed_at_ms != null || blindSubs().some((s: any) => s.blind_revealed_at_ms != null),
            );

            return (
              <View
                style={{
                  borderRadius: 10,
                  borderWidth: 1,
                  borderColor: msg().pinned
                    ? look().palette.accent
                    : msg().kind === "blind_prompt"
                      ? look().palette.need
                      : look().palette.hair,
                  backgroundColor: look().palette.surface,
                  padding: 12,
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
                  <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 6 }}>
                    <Text
                      style={{
                        fontSize: look().type.small.size,
                        fontWeight: look().type.ui_medium.weight,
                        color: look().palette.text,
                      }}
                    >
                      {senderLabel()}
                    </Text>
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                      #{msg().seq}
                    </Text>
                    <If when={msg().kind !== "chat"}>
                      <View
                        style={{
                          paddingLeft: 5,
                          paddingRight: 5,
                          paddingVertical: 1,
                          borderRadius: 4,
                          backgroundColor: look().palette.soft,
                        }}
                      >
                        <Text style={{ fontSize: 10, color: look().palette.muted }}>
                          {msg().kind}
                          {msg().task_status ? ` · ${msg().task_status}` : ""}
                        </Text>
                      </View>
                    </If>
                    <If when={Boolean(msg().auto_bridged)}>
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                        · {t("timeline.auto_bridged")}
                      </Text>
                    </If>
                  </View>

                  <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4 }}>
                    <If when={sender() != null}>
                      <SmeltButton
                        quiet
                        small
                        label={t("timeline.view_session")}
                        onPress={() => inspectMember(sender()!.id)}
                      />
                    </If>
                    <SmeltButton
                      quiet
                      small
                      label={msg().pinned ? "Unpin" : "Pin"}
                      onPress={() => void toggleMessagePinned(msg().seq, !msg().pinned)}
                    />
                  </View>
                </View>

                <Text
                  selectable
                  style={{
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    color: look().palette.text,
                  }}
                >
                  {msg().content}
                </Text>

                {/* 盲评并发对比卡片 */}
                <If when={msg().kind === "blind_prompt"}>
                  <View
                    style={{
                      marginTop: 6,
                      paddingTop: 8,
                      borderTopWidth: 1,
                      borderTopColor: look().palette.hair,
                      display: "flex",
                      flexDirection: "column",
                      gap: 8,
                    }}
                  >
                    <View
                      style={{
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        justifyContent: "space-between",
                      }}
                    >
                      <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
                        {blindRevealed() ? t("timeline.blind_revealed") : t("timeline.blind_collecting")} (
                        {blindSubs().length}/{(msg().mentions ?? []).length})
                      </Text>
                      <If when={!blindRevealed()}>
                        <SmeltButton
                          small
                          primary
                          label={t("timeline.reveal_now")}
                          onPress={() => void revealBlindRound(msg().seq)}
                        />
                      </If>
                    </View>
                    <If when={blindSubs().length > 0}>
                      <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
                        <Each of={blindSubs()} key={(sub: any) => sub.id}>
                          {(sub: () => any) => {
                            const author = computed(() => memberById(sub().sender_member_id));
                            return (
                              <View
                                style={{
                                  flex: 1,
                                  minWidth: 220,
                                  padding: 10,
                                  borderRadius: 8,
                                  backgroundColor: look().palette.soft,
                                  display: "flex",
                                  flexDirection: "column",
                                  gap: 4,
                                }}
                              >
                                <Text
                                  style={{
                                    fontSize: look().type.small.size,
                                    fontWeight: "600",
                                    color: look().palette.text,
                                  }}
                                >
                                  @{author()?.alias ?? "member"}
                                </Text>
                                <Text
                                  selectable
                                  style={{
                                    fontSize: look().type.small.size,
                                    color: look().palette.text,
                                  }}
                                >
                                  {sub().content}
                                </Text>
                              </View>
                            );
                          }}
                        </Each>
                      </View>
                    </If>
                  </View>
                </If>

                {/* 权限水位放行确认卡片 */}
                <If when={pendingApprovals().length > 0}>
                  <View style={{ display: "flex", flexDirection: "column", gap: 6, marginTop: 4 }}>
                    <Each of={pendingApprovals()} key={(mn: any) => `${msg().id}:${mn.target_member_id}`}>
                      {(mn: () => any) => {
                        const target = computed(() => memberById(mn().target_member_id));
                        return (
                          <View
                            style={{
                              padding: 8,
                              borderRadius: 8,
                              borderWidth: 1,
                              borderColor: look().palette.need,
                              backgroundColor: look().palette.soft,
                              display: "flex",
                              flexDirection: "row",
                              alignItems: "center",
                              justifyContent: "space-between",
                              gap: 8,
                            }}
                          >
                            <Text style={{ fontSize: look().type.small.size, color: look().palette.text, flex: 1 }}>
                              {t("timeline.approval_title", { target: target()?.alias ?? mn().target_member_id })}
                            </Text>
                            <View style={{ display: "flex", flexDirection: "row", gap: 6 }}>
                              <SmeltButton
                                small
                                primary
                                label={t("timeline.approve_once")}
                                onPress={() =>
                                  void decideMentionApproval(msg().id, mn().target_member_id, true, false)
                                }
                              />
                              <SmeltButton
                                small
                                label={t("timeline.approve_chain")}
                                onPress={() =>
                                  void decideMentionApproval(msg().id, mn().target_member_id, true, true)
                                }
                              />
                              <SmeltButton
                                small
                                quiet
                                danger
                                label={t("timeline.reject")}
                                onPress={() =>
                                  void decideMentionApproval(msg().id, mn().target_member_id, false, false)
                                }
                              />
                            </View>
                          </View>
                        );
                      }}
                    </Each>
                  </View>
                </If>

                {/* mention 模式未 @ 成员时提供一键唤醒筹码 */}
                <If when={isUnmentionedUserChat()}>
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      flexWrap: "wrap",
                      alignItems: "center",
                      gap: 4,
                      marginTop: 2,
                    }}
                  >
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                      {t("timeline.wake_chip")}
                    </Text>
                    <Each of={members()} key={(m: any) => m.id}>
                      {(m: () => any) => (
                        <SmeltButton
                          quiet
                          small
                          label={`@${m().alias}`}
                          onPress={() => void quickWakeFromMessage(msg().seq, m().alias)}
                        />
                      )}
                    </Each>
                    <SmeltButton
                      quiet
                      small
                      label="@all"
                      onPress={() => void quickWakeFromMessage(msg().seq, "all")}
                    />
                  </View>
                </If>
              </View>
            );
          }}
        </Each>
      </View>
    </Scroll>
  );
}

function GroupComposer() {
  const look = currentTheme;
  const members = computed(() => activeDetail()?.members ?? []);
  const mentionQuery = computed(() => activeMentionQuery(composerText()));
  const candidates = computed(() =>
    mentionQuery() != null ? filterMentionCandidates(members(), mentionQuery()!) : [],
  );

  return (
    <View
      style={{
        paddingLeft: 18,
        paddingRight: 18,
        paddingTop: 10,
        paddingBottom: 14,
        borderTopWidth: 1,
        borderTopColor: look().palette.hair,
        display: "flex",
        flexDirection: "column",
        gap: 6,
        flexShrink: 0,
      }}
    >
      <If when={candidates().length > 0}>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            flexWrap: "wrap",
            gap: 4,
            padding: 6,
            borderRadius: 8,
            borderWidth: 1,
            borderColor: look().palette.hair,
            backgroundColor: look().palette.surface,
          }}
        >
          <Each of={candidates()} key={(c: any) => c.alias}>
            {(c: () => any) => (
              <SmeltButton
                quiet
                small
                label={`@${c().alias}`}
                onPress={() => composerText.set(applyMentionCompletion(composerText(), c().alias))}
              />
            )}
          </Each>
        </View>
      </If>

      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
        <Input
          class={`${INPUT_CLASS} flex-1 min-w-0`}
          value={composerText()}
          placeholder={t("composer.placeholder")}
          onChangeText={(next: string) => composerText.set(next)}
          onSubmit={() => void sendComposerMessage()}
        />
        <SmeltButton
          primary
          disabled={sending() || composerText().trim().length === 0}
          loading={sending()}
          label={t("composer.send")}
          onPress={() => void sendComposerMessage()}
        />
      </View>
    </View>
  );
}

function RightDrawer() {
  const look = currentTheme;
  const detail = activeDetail;
  const ref = selectedGroup;
  const members = computed(() => detail()?.members ?? []);
  const inspected = computed(() => {
    const id = inspectedMemberId();
    return members().find((m: any) => m.id === id) ?? members()[0] ?? null;
  });
  const tasks = computed(() => activeMessages().filter((m: any) => m.kind === "task"));

  return (
    <View
      style={{
        width: 420,
        borderLeftWidth: 1,
        borderLeftColor: look().palette.hair,
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
        flexShrink: 0,
      }}
    >
      <View
        style={{
          height: 40,
          paddingLeft: 10,
          paddingRight: 10,
          borderBottomWidth: 1,
          borderBottomColor: look().palette.hair,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 4,
          flexShrink: 0,
        }}
      >
        <View style={{ display: "flex", flexDirection: "row", gap: 4 }}>
          <SmeltButton
            quiet
            small
            toggled={drawerTab() === "session"}
            label={t("drawer.tab_session")}
            onPress={() => drawerTab.set("session")}
          />
          <SmeltButton
            quiet
            small
            toggled={drawerTab() === "tasks"}
            label={`${t("drawer.tab_tasks")} (${tasks().length})`}
            onPress={() => drawerTab.set("tasks")}
          />
          <SmeltButton
            quiet
            small
            toggled={drawerTab() === "branches"}
            label={t("drawer.tab_branches")}
            onPress={() => drawerTab.set("branches")}
          />
        </View>
        <SmeltButton quiet small label={t("drawer.close")} onPress={() => drawerOpen.set(false)} />
      </View>

      <If when={drawerTab() === "session"}>
        <If
          when={inspected() != null}
          else={
            <View style={{ padding: 16 }}>
              <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
                Select a member pill to inspect their private conversation.
              </Text>
            </View>
          }
        >
          <View style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column" }}>
            <SmeltConversation
              machine={inspected()!.runtime_id ?? ref()?.machine ?? null}
              sessionId={inspected()!.session_id}
              embedded
              title={`@${inspected()!.alias}`}
              style={{ flex: 1 }}
            />
          </View>
        </If>
      </If>

      <If when={drawerTab() === "tasks"}>
        <Scroll style={{ flex: 1, minHeight: 0 }}>
          <View style={{ padding: 12, display: "flex", flexDirection: "column", gap: 8 }}>
            <If
              when={tasks().length > 0}
              else={
                <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
                  {t("drawer.no_tasks")}
                </Text>
              }
            >
              <Each of={tasks()} key={(item: any) => item.id}>
                {(item: () => any) => (
                  <View
                    style={{
                      padding: 10,
                      borderRadius: 8,
                      borderWidth: 1,
                      borderColor: look().palette.hair,
                      backgroundColor: look().palette.surface,
                      display: "flex",
                      flexDirection: "column",
                      gap: 4,
                    }}
                  >
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                      #{item().seq} · {item().task_status ?? "open"}
                    </Text>
                    <Text style={{ fontSize: look().type.small.size, color: look().palette.text }}>
                      {item().content}
                    </Text>
                  </View>
                )}
              </Each>
            </If>
          </View>
        </Scroll>
      </If>

      <If when={drawerTab() === "branches"}>
        <Scroll style={{ flex: 1, minHeight: 0 }}>
          <View style={{ padding: 12, display: "flex", flexDirection: "column", gap: 10 }}>
            <Each of={members()} key={(m: any) => m.id}>
              {(m: () => any) => {
                const diff = computed(() => worktreeDiffs()[m().id] ?? null);
                const busy = computed(() => worktreeBusyMember() === m().id);
                return (
                  <View
                    style={{
                      padding: 10,
                      borderRadius: 8,
                      borderWidth: 1,
                      borderColor: look().palette.hair,
                      backgroundColor: look().palette.surface,
                      display: "flex",
                      flexDirection: "column",
                      gap: 6,
                    }}
                  >
                    <View style={{ display: "flex", flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
                      <Text style={{ fontSize: look().type.small.size, fontWeight: "600", color: look().palette.text }}>
                        @{m().alias} ({m().branch || m().workspace_mode})
                      </Text>
                      <SmeltButton
                        quiet
                        small
                        loading={busy()}
                        label={t("drawer.load_diff")}
                        onPress={() => void loadMemberDiff(m().id)}
                      />
                    </View>
                    <View style={{ display: "flex", flexDirection: "row", gap: 6, flexWrap: "wrap" }}>
                      <SmeltButton
                        small
                        disabled={busy()}
                        label={t("drawer.merge_integration")}
                        onPress={() => void mergeMemberBranch(m().id, "integration")}
                      />
                      <SmeltButton
                        small
                        quiet
                        disabled={busy()}
                        label={t("drawer.merge_base")}
                        onPress={() => void mergeMemberBranch(m().id, "base")}
                      />
                    </View>
                    <If when={diff() != null}>
                      <Text
                        selectable
                        style={{
                          fontSize: look().type.text_xsmall.size,
                          color: look().palette.muted,
                        }}
                      >
                        {diff()?.merged_commit
                          ? `Merged → ${diff().merged_target} (${diff().merged_commit.slice(0, 8)})`
                          : diff()?.diff_stat || "(clean working tree)"}
                      </Text>
                    </If>
                  </View>
                );
              }}
            </Each>
          </View>
        </Scroll>
      </If>
    </View>
  );
}

function CreateGroupDialog() {
  const look = currentTheme;
  const draft = createDraft;
  const machines = computed(() => availableMachines());

  return (
    <SmeltDialog open={draft() != null} onOpenChange={(open) => !open && closeCreateGroupDialog()}>
      <If when={draft() != null}>
        <View class={DIALOG_SURFACE_CLASS} style={{ width: 460, padding: 20, gap: 14 }}>
          <Text
            style={{
              fontSize: look().type.title.size,
              lineHeight: look().type.title.line_height,
              fontWeight: look().type.title.weight,
              color: look().palette.text,
            }}
          >
            {t("create.title")}
          </Text>

          <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
              {t("create.name_label")}
            </Text>
            <Input
              class={INPUT_CLASS}
              value={draft()!.title}
              placeholder="e.g. OAuth Refactor Trio"
              onChangeText={(v: string) => patchCreateDraft({ title: v })}
            />
          </View>

          <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
              {t("create.host_label")}
            </Text>
            <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
              <Each of={machines()} key={(m: any) => m.machine ?? "local"}>
                {(m: () => any) => (
                  <SmeltButton
                    small
                    toggled={(draft()!.hostMachine ?? null) === (m().machine ?? null)}
                    label={m().name}
                    onPress={() => patchCreateDraft({ hostMachine: m().machine ?? null })}
                  />
                )}
              </Each>
            </View>
          </View>

          <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
              {t("create.template_label")}
            </Text>
            <View style={{ display: "flex", flexDirection: "row", gap: 6, flexWrap: "wrap" }}>
              <Each of={GROUP_TEMPLATES} key={(tpl: any) => tpl.id}>
                {(tpl: () => any) => (
                  <SmeltButton
                    small
                    toggled={draft()!.templateId === tpl().id}
                    label={
                      tpl().id === "dev-trio"
                        ? t("create.template_dev_trio")
                        : t("create.template_blind")
                    }
                    onPress={() => patchCreateDraft({ templateId: tpl().id })}
                  />
                )}
              </Each>
            </View>
          </View>

          <If when={Boolean(draft()!.error)}>
            <Text style={{ fontSize: look().type.small.size, color: look().palette.bad }}>
              {draft()!.error ?? ""}
            </Text>
          </If>

          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton quiet label={t("create.cancel")} onPress={closeCreateGroupDialog} />
            <SmeltButton
              primary
              loading={draft()!.submitting}
              label={t("create.submit")}
              onPress={() => void submitCreateGroup()}
            />
          </View>
        </View>
      </If>
    </SmeltDialog>
  );
}

function AddMemberDialog() {
  const look = currentTheme;
  const draft = addMemberDraft;
  const agents = computed(() => availableConversationAgents());

  return (
    <SmeltDialog open={draft() != null} onOpenChange={(open) => !open && closeAddMemberDialog()}>
      <If when={draft() != null}>
        <View class={DIALOG_SURFACE_CLASS} style={{ width: 440, padding: 20, gap: 12 }}>
          <Text style={{ fontSize: look().type.title.size, fontWeight: look().type.title.weight, color: look().palette.text }}>
            {t("header.add_member")}
          </Text>
          <Input
            class={INPUT_CLASS}
            value={draft()!.alias}
            placeholder="Alias (e.g. codex, claude, security)"
            onChangeText={(v: string) => patchAddMemberDraft({ alias: v })}
          />
          <If when={draft()!.ownership === "group_owned"}>
            <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
              <Each of={agents()} key={(a: any) => a.id}>
                {(a: () => any) => (
                  <SmeltButton
                    small
                    toggled={draft()!.agentOptionId === a().id}
                    label={a().label}
                    onPress={() => patchAddMemberDraft({ agentOptionId: a().id })}
                  />
                )}
              </Each>
            </View>
          </If>
          <If when={Boolean(draft()!.error)}>
            <Text style={{ fontSize: look().type.small.size, color: look().palette.bad }}>
              {draft()!.error ?? ""}
            </Text>
          </If>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton quiet label={t("create.cancel")} onPress={closeAddMemberDialog} />
            <SmeltButton
              primary
              loading={draft()!.submitting}
              label={t("header.add_member")}
              onPress={() => void submitAddMember()}
            />
          </View>
        </View>
      </If>
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
        flexDirection: "column",
      }}
    >
      <If when={Boolean(pageError())}>
        <View
          style={{
            paddingLeft: 18,
            paddingRight: 18,
            paddingTop: 6,
            paddingBottom: 6,
            backgroundColor: look().palette.soft,
            borderBottomWidth: 1,
            borderBottomColor: look().palette.bad,
          }}
        >
          <Text style={{ fontSize: look().type.small.size, color: look().palette.bad }}>
            {pageError() ?? ""}
          </Text>
        </View>
      </If>

      <If
        when={selectedGroup() != null && activeDetail() != null}
        else={
          <View
            style={{
              flex: 1,
              alignItems: "center",
              justifyContent: "center",
              gap: 12,
              padding: 24,
            }}
          >
            <SmeltIcon path="icons/users.svg" size={28} color={look().palette.faint} />
            <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
              {t("timeline.empty_select")}
            </Text>
            <SmeltButton
              primary
              label={t("create.title")}
              onPress={() => openCreateGroupDialog({ hostMachine: null })}
            />
          </View>
        }
      >
        <GroupHeader />
        <MemberStatusRail />
        <View style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "row" }}>
          <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column" }}>
            <MessageTimeline />
            <GroupComposer />
          </View>
          <If when={drawerOpen()}>
            <RightDrawer />
          </If>
        </View>
      </If>

      <CreateGroupDialog />
      <AddMemberDialog />
    </View>
  );
}

export default main;
