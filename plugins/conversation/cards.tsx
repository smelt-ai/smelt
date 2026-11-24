// 输入框上方的交互卡片：权限审批卡、Elicitation 提问表单卡、记忆变更审核卡。
import { computed, Scroll, signal, Text, View } from "nsh";
import { Input } from "nsh/base";
import { call, openUrl, SmeltButton, SmeltDisclosure, SmeltDisclosureChevron, SmeltIcon, theme } from "smelt/sdk";
import { ct } from "./i18n.ts";

function isAllowPermissionKind(kind: unknown): boolean {
  return (
    kind === "AllowOnce" ||
    kind === "AllowAlways" ||
    kind === "allow_once" ||
    kind === "allow_always"
  );
}

function elicitFieldOptions(field: any): { options: any[]; multi: boolean } | null {
  const k = field?.kind;
  if (!k || typeof k !== "object") return null;
  if (Array.isArray(k.Select)) return { options: k.Select, multi: false };
  if (Array.isArray(k.MultiSelect)) return { options: k.MultiSelect, multi: true };
  return null;
}

function elicitOptionDisplayLabel(opt: any): string {
  if (opt?.flag === true) return ct("choice.yes");
  if (opt?.flag === false) return ct("choice.no");
  return String(opt?.label ?? "");
}

export function PermissionTray(props: {
  permissions: () => any[];
  submitting: () => boolean;
  onSelect: (toolCallId: string, optionId: string) => void;
}) {
  const look = theme;
  const first = computed(() => props.permissions()[0] ?? null);
  const remaining = computed(() => Math.max(0, props.permissions().length - 1));
  const details = computed(() => first()?.details ?? null);

  const title = computed(() => {
    const d = details();
    if (!d || typeof d !== "object") return ct("approval.title");
    if (d.kind === "command") return ct("approval.kind.command");
    if (d.kind === "file_change") return ct("approval.kind.file_change");
    if (d.kind === "permissions") return ct("approval.kind.permissions");
    return ct("approval.title");
  });

  const primaryIndex = computed(() => {
    const opts: any[] = first()?.options ?? [];
    const ix = opts.findIndex((o) => isAllowPermissionKind(o?.kind));
    return ix >= 0 ? ix : 0;
  });

  return (
    <If when={first() != null}>
      <View
        style={{
          width: "100%",
          paddingHorizontal: 18,
          paddingTop: 14,
          paddingBottom: 12,
          gap: 10,
          borderBottomWidth: 1,
          borderBottomColor: look().palette.hair,
        }}
      >
        <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <View
            style={{
              width: 7,
              height: 7,
              borderRadius: 4,
              backgroundColor: look().palette.need,
            }}
          />
          <Text
            style={{
              fontSize: look().type.ui_medium?.size ?? 13,
              fontWeight: look().type.ui_medium?.weight ?? 530,
              color: look().palette.text,
            }}
          >
            {title()}
          </Text>
          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted, flex: 1 }}>
            {ct("approval.waiting")}
          </Text>
          <If when={remaining() > 0}>
            <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
              {ct("approval.remaining", { count: remaining() })}
            </Text>
          </If>
        </View>

        <Scroll style={{ maxHeight: 160, paddingLeft: 15 }}>
          <View style={{ gap: 6 }}>
            <If when={details()?.kind === "command"}>
              <View class="px-3 py-2 rounded-lg border border-border bg-sidebar">
                <Text
                  class="font-mono text-foreground"
                  style={{ fontSize: look().type.text_xsmall.size }}
                >
                  {details()?.command ?? ""}
                </Text>
              </View>
              <If when={Boolean(details()?.reason)}>
                <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                  {details()?.reason ?? ""}
                </Text>
              </If>
              <If when={Boolean(details()?.cwd)}>
                <Text
                  style={{
                    fontFamily: look().fonts?.mono,
                    fontSize: look().type.text_xsmall.size,
                    color: look().palette.faint,
                  }}
                >
                  {ct("approval.cwd", { path: details()?.cwd ?? "" })}
                </Text>
              </If>
            </If>
            <If when={details()?.kind === "file_change"}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {details()?.reason ?? first()?.question ?? ""}
              </Text>
              <If when={Boolean(details()?.grant_root)}>
                <Text
                  style={{
                    fontFamily: look().fonts?.mono,
                    fontSize: look().type.text_xsmall.size,
                    color: look().palette.faint,
                  }}
                >
                  {ct("approval.root", { path: details()?.grant_root ?? "" })}
                </Text>
              </If>
            </If>
            <If when={details()?.kind === "permissions"}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {details()?.summary ?? ""}
              </Text>
            </If>
            <If when={!details() || details()?.kind === "generic" || !details()?.kind}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {first()?.question ?? ""}
              </Text>
            </If>
          </View>
        </Scroll>

        <If
          when={!props.submitting()}
          else={
            <View style={{ paddingLeft: 16, paddingVertical: 4 }}>
              <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
                {ct("status.working")}
              </Text>
            </View>
          }
        >
          <View style={{ paddingLeft: 12, gap: 3 }}>
            <Each of={first()?.options ?? []}>
              {(opt, idx) => {
                const isPrimary = computed(() => idx() === primaryIndex());
                return (
                  <View
                    onPress={() => {
                      const p = first();
                      if (p) props.onSelect(p.tool_call_id, opt().option_id);
                    }}
                    class="hover:bg-accent"
                    style={{
                      height: 32,
                      paddingHorizontal: 10,
                      borderRadius: 8,
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 8,
                      backgroundColor: isPrimary() ? look().palette.row_selected : "transparent",
                    }}
                  >
                    <Text
                      style={{
                        flex: 1,
                        minWidth: 0,
                        fontSize: look().type.ui.size,
                        fontWeight: isPrimary() ? 530 : 440,
                        color: look().palette.text,
                        numberOfLines: 1,
                      }}
                    >
                      {opt().name}
                    </Text>
                    <If when={isPrimary()}>
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                        ⌘⏎
                      </Text>
                    </If>
                  </View>
                );
              }}
            </Each>
          </View>
        </If>
      </View>
    </If>
  );
}

export function ElicitationCard(props: {
  elicitation: () => any | null;
  onChoose: (fieldIx: number, optIx: number) => void;
  onText: (fieldIx: number, value: string) => void;
  onSubmit: () => void;
  onDismiss: () => void;
}) {
  const look = theme;
  const item = () => props.elicitation();

  return (
    <If when={item() != null}>
      <View
        style={{
          width: "100%",
          paddingHorizontal: 14,
          paddingTop: 12,
          paddingBottom: 10,
          gap: 10,
          borderBottomWidth: 1,
          borderBottomColor: look().palette.hair,
        }}
      >
        <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <View
            style={{
              width: 7,
              height: 7,
              borderRadius: 4,
              backgroundColor: look().palette.need,
            }}
          />
          <Text
            style={{
              flex: 1,
              fontSize: look().type.small_medium?.size ?? 12,
              fontWeight: look().type.small_medium?.weight ?? "500",
              color: look().palette.text,
            }}
          >
            {item()?.message || ct("elicit.title")}
          </Text>
          <SmeltButton quiet small label={ct("elicit.skip")} onPress={props.onDismiss} />
        </View>

        <Scroll style={{ maxHeight: 260 }}>
          <View style={{ gap: 10, paddingLeft: 14 }}>
            <Each of={item()?.fields ?? []}>
              {(field, fieldIdx) => {
                const optGroup = computed(() => elicitFieldOptions(field()));
                const isTextKind = computed(() => Boolean(field()?.kind?.Text));
                const externalUrl = computed(() =>
                  typeof field()?.kind?.ExternalUrl === "string" ? field().kind.ExternalUrl : null,
                );
                const chosenList = computed(() => {
                  const chosen = item()?.chosen ?? {};
                  const arr = chosen[String(fieldIdx())] ?? chosen[fieldIdx()];
                  return Array.isArray(arr) ? arr : [];
                });
                const currentText = computed(() => {
                  const tv = item()?.text_values ?? {};
                  return String(tv[String(fieldIdx())] ?? tv[fieldIdx()] ?? "");
                });
                return (
                  <View style={{ gap: 6 }}>
                    <If when={Boolean(field().title || field().key)}>
                      <Text style={{ fontSize: look().type.small.size, color: look().palette.text }}>
                        {field().title || field().key}
                      </Text>
                    </If>
                    <If when={externalUrl() != null}>
                      <SmeltButton
                        quiet
                        small
                        icon="icons/external-link.svg"
                        label={externalUrl() ?? ""}
                        onPress={() => {
                          const u = externalUrl();
                          if (u) void openUrl(u);
                        }}
                      />
                    </If>
                    <If when={optGroup() != null}>
                      <View style={{ gap: 3 }}>
                        <Each of={optGroup()?.options ?? []}>
                          {(opt, optIdx) => {
                            const selected = computed(() => chosenList().includes(optIdx()));
                            return (
                              <View
                                onPress={() => props.onChoose(fieldIdx(), optIdx())}
                                style={{
                                  paddingHorizontal: 10,
                                  paddingVertical: 6,
                                  borderRadius: look().radius.row ?? 6,
                                  borderWidth: 1,
                                  borderColor: selected() ? look().palette.ink : look().palette.hair,
                                  backgroundColor: selected() ? look().palette.row_selected : "transparent",
                                  gap: 2,
                                }}
                              >
                                <Text style={{ fontSize: look().type.small.size, color: look().palette.text }}>
                                  {elicitOptionDisplayLabel(opt())}
                                </Text>
                                <If when={Boolean(opt().description)}>
                                  <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                                    {opt().description}
                                  </Text>
                                </If>
                              </View>
                            );
                          }}
                        </Each>
                      </View>
                    </If>
                    <If when={isTextKind() || Boolean(field().allow_custom_input)}>
                      <Input
                        value={currentText()}
                        placeholder={ct("elicit.custom_placeholder")}
                        masked={Boolean(field()?.kind?.Text?.secret)}
                        onChange={({ value }: any) => props.onText(fieldIdx(), value)}
                        style={{
                          height: 32,
                          paddingHorizontal: 10,
                          borderRadius: 6,
                          borderWidth: 1,
                          borderColor: look().palette.hair,
                          backgroundColor: look().palette.bg,
                          color: look().palette.text,
                          placeholderColor: look().palette.faint,
                          fontSize: look().type.small.size,
                        }}
                      />
                    </If>
                  </View>
                );
              }}
            </Each>
          </View>
        </Scroll>

        <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
          <SmeltButton primary small label={ct("elicit.submit")} onPress={props.onSubmit} />
        </View>
      </View>
    </If>
  );
}

export function MemoryChangesCard(props: {
  machine: () => string | null;
  changes: () => any | null;
  onOpenAgentMemory?: (agentId: string) => void;
}) {
  const look = theme;
  const dismissedId = signal<number | null>(null);
  const expandedKey = signal<string | null>(null);
  const deleteArmedKey = signal<string | null>(null);
  const busyKey = signal<string | null>(null);
  const errorMsg = signal<string | null>(null);

  const visible = computed(() => {
    const ch = props.changes();
    if (!ch || !Array.isArray(ch.entries) || ch.entries.length === 0) return null;
    if (ch.id != null && dismissedId() === ch.id) return null;
    return ch;
  });

  const runAction = async (kind: "approve" | "delete", entry: any) => {
    const ch = visible();
    if (!ch) return;
    const key = `${entry.layer}:${entry.file_name}`;
    busyKey.set(key);
    errorMsg.set(null);
    try {
      const method = kind === "approve" ? "agent.memory.approve" : "agent.memory.delete";
      await call(props.machine(), method, {
        agent_id: ch.agent_id,
        layer: entry.layer,
        file_name: entry.file_name,
      });
      deleteArmedKey.set(null);
    } catch (err: any) {
      errorMsg.set(err?.message ?? String(err));
    } finally {
      busyKey.set(null);
    }
  };

  return (
    <If when={visible() != null}>
      <View style={{ width: "100%", alignItems: "center", paddingHorizontal: 16, paddingTop: 8 }}>
        <View
          class="w-full rounded-[14px] border border-border bg-card shadow-md overflow-hidden"
          style={{ maxWidth: 768 }}
        >
          <View
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 8,
              paddingLeft: 12,
              paddingRight: 6,
              paddingTop: 6,
              paddingBottom: 4,
            }}
          >
            <SmeltIcon path="icons/brain.svg" size={14} color={look().palette.muted} />
            <Text style={{ flex: 1, fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
              {ct("memory.summary", { count: visible()?.entries.length ?? 0 })}
            </Text>
            <If when={Boolean(props.onOpenAgentMemory)}>
              <SmeltButton
                quiet
                small
                label={ct("memory.page")}
                onPress={() => {
                  const id = visible()?.agent_id;
                  if (id && props.onOpenAgentMemory) props.onOpenAgentMemory(id);
                }}
              />
            </If>
            <SmeltButton
              quiet
              small
              icon="icons/close.svg"
              onPress={() => {
                const id = visible()?.id;
                if (id != null) dismissedId.set(id);
              }}
            />
          </View>

          <Scroll style={{ maxHeight: 220 }}>
            <View style={{ padding: 4, gap: 2 }}>
              <Each of={visible()?.entries ?? []}>
                {(entry) => {
                  const rowKey = computed(() => `${entry().layer}:${entry().file_name}`);
                  const expanded = computed(() => expandedKey() === rowKey());
                  const armed = computed(() => deleteArmedKey() === rowKey());
                  const busy = computed(() => busyKey() === rowKey());
                  return (
                    <SmeltDisclosure
                      open={expanded() && Boolean(entry().body || entry().content)}
                      style={{ paddingHorizontal: 8, paddingVertical: 6 }}
                      content={
                        <Scroll style={{ maxHeight: 140, marginTop: 4, paddingLeft: 18 }}>
                          <Text
                            style={{
                              fontSize: look().type.text_xsmall.size,
                              color: look().palette.muted,
                              fontFamily: look().fonts?.mono,
                            }}
                          >
                            {entry().body ?? entry().content ?? ""}
                          </Text>
                        </Scroll>
                      }
                    >
                      <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
                        <View
                          onPress={() => expandedKey.set(expanded() ? null : rowKey())}
                          style={{ flex: 1, flexDirection: "row", alignItems: "center", gap: 6, minWidth: 0 }}
                        >
                          <SmeltDisclosureChevron
                            open={expanded()}
                            size={12}
                            color={look().palette.faint}
                          />
                          <View style={{ flex: 1, minWidth: 0 }}>
                            <Text
                              style={{
                                fontSize: look().type.small.size,
                                color: look().palette.text,
                                numberOfLines: 1,
                              }}
                            >
                              {entry().description || entry().name}
                            </Text>
                            <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                              {[
                                entry().description ? entry().name : null,
                                entry().created ? ct("memory.created") : ct("memory.changed"),
                                entry().layer === "user" ? ct("memory.global") : null,
                                entry().pending_reason ? ct("memory.pending") : null,
                              ]
                                .filter(Boolean)
                                .join(" · ")}
                            </Text>
                          </View>
                        </View>
                        <If when={Boolean(entry().pending_reason)}>
                          <SmeltButton
                            small
                            disabled={busy()}
                            label={ct("memory.approve")}
                            onPress={() => void runAction("approve", entry())}
                          />
                        </If>
                        <SmeltButton
                          quiet={!armed()}
                          danger={armed()}
                          small
                          disabled={busy()}
                          label={armed() ? ct("memory.confirm_delete") : ct("memory.delete")}
                          onPress={() => {
                            if (armed()) {
                              void runAction("delete", entry());
                            } else {
                              deleteArmedKey.set(rowKey());
                            }
                          }}
                        />
                      </View>
                    </SmeltDisclosure>
                  );
                }}
              </Each>
            </View>
          </Scroll>

          <If when={Boolean(errorMsg())}>
            <Text style={{ paddingHorizontal: 12, paddingBottom: 8, fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
              {errorMsg() ?? ""}
            </Text>
          </If>
        </View>
      </View>
    </If>
  );
}
