// 会话列表插件（NSH A2 独立 Guest VM 版）：
// 业务逻辑与 SQLite 存储 100% 复用 `plugins/session-list/` 的纯 JS 模块，界面完全用 NSH TSX 拼装，
// 并通过跨 VM 插槽 `<Slot>` 组合行首头像插件（例如 buddies），互不阻塞、独立热重载与故障隔离。

import { computed, nativeComponent, Scroll, signal, Text, View } from "nsh";
import { ContextMenu, Input, Menu } from "nsh/base";
import { Slot } from "smelt/slots";

import * as actions from "./actions.js";
import {
  clipboardWrite,
  ContributedMenuItem,
  fetchContextMenuItems,
  list,
  slotViews,
  space,
  t,
  theme,
  tick,
} from "./bridge.ts";
import { PROJECT_MENU, SESSION_AVATAR, SESSION_MENU } from "./contract.js";
import { bucket_label } from "./group.js";
import {
  ConfirmDialog,
  ConfirmRequest,
  ContributedMenuItems,
  GroupingMenu,
  MENU_ITEM_CLASS,
  MenuDivider,
  MenuEntry,
  MenuSectionTitle,
  POPUP_CLASS,
} from "./menus.tsx";
import { pick_avatar, same_conversation, selected_session } from "./model.js";
import {
  bucket_flip,
  CONVERSATIONS_KEY,
  machine_flip,
  project_flip,
  project_visible,
  row_tally,
  session_flip,
  workset_flip,
} from "./sidebar.js";
import { fail_key } from "./state.js";
import { relative_time, span } from "./time.js";
import { icon, message } from "./util.js";

const ROW_MD = 32;
const AVATAR = 36;
const AVATAR_ROW = 44;
const LEAD = 16;
const BADGE = 13;
const INDENT_STEP = 14;
const DRAG_THRESHOLD = 4;

const Icon = nativeComponent<{ path: string; color: string }, {}>("smelt.icon", {
  props: ["path", "color"],
  events: [],
});

interface RenameState {
  target:
    | { kind: "conversation"; key: { machine: string | null; id: string } }
    | { kind: "workset"; machine: string | null; id: string };
  original: string;
  value: string;
}

interface DragState {
  kind: "session" | "project";
  key: string;
  payload: any;
  startY: number;
  active: boolean;
  overKey: string | null;
  before: boolean;
}

function dotColor(palette: ReturnType<typeof theme>["palette"], status: string): string {
  if (status === "needs_you") return palette.need;
  if (status === "running") return palette.run;
  return palette.faint;
}

function lightColor(palette: ReturnType<typeof theme>["palette"], light: string): string {
  if (light === "good") return palette.ok;
  if (light === "degraded") return palette.need;
  return palette.bad;
}

function rowMuted(palette: ReturnType<typeof theme>["palette"], selected: boolean): string {
  return selected ? palette.muted_selected : palette.muted;
}

function rowFaint(palette: ReturnType<typeof theme>["palette"], selected: boolean): string {
  return selected ? palette.faint_selected : palette.faint;
}

function iconPath(rowIcon: any): string {
  if (rowIcon?.kind === "provider") return rowIcon.path;
  if (rowIcon?.kind === "terminal") return icon("terminal");
  return icon("message-square");
}

function indent(level: number): number {
  return space(2) + level * INDENT_STEP;
}

function rowMeta(row: any, selected: boolean) {
  const palette = theme().palette;
  const nowSec = Math.floor(Date.now() / 1000);
  const unread = row_tally(row).unread > 0;
  if (row.unavailable != null) {
    return { text: row.unavailable, color: rowFaint(palette, selected), unread };
  }
  if (row.status === "needs_you") {
    return { text: t("bucket.needs_you"), color: palette.need, unread };
  }
  if (row.status === "running" && row.turn_started_at > 0) {
    return { text: span(Math.max(nowSec - row.turn_started_at, 0), t), color: palette.run, unread };
  }
  return { text: relative_time(row.activity, nowSec, t), color: rowFaint(palette, selected), unread };
}

function decodeFileUris(uris: string[]): string[] {
  const paths: string[] = [];
  for (const raw of uris) {
    if (raw.startsWith("file://")) {
      try {
        paths.push(decodeURIComponent(raw.slice("file://".length)));
      } catch {
        paths.push(raw.slice("file://".length));
      }
    } else if (raw.length > 0) {
      paths.push(raw);
    }
  }
  return paths;
}

function isRenamingTarget(rename: RenameState | null, target: RenameState["target"]): boolean {
  if (!rename || rename.target.kind !== target.kind) return false;
  if (target.kind === "conversation" && rename.target.kind === "conversation") {
    return same_conversation(rename.target.key, target.key);
  }
  if (target.kind === "workset" && rename.target.kind === "workset") {
    return rename.target.machine === target.machine && rename.target.id === target.id;
  }
  return false;
}

function FailureMark(props: { target: any }) {
  const look = () => theme();
  const failure = () => {
    tick();
    return list.failures.get(fail_key(props.target)) ?? null;
  };
  return (
    <>
      {failure() ? (
        <View
          role="button"
          accessibilityLabel={t("error.failed", { what: failure()!.what, error: failure()!.error })}
          class="items-center justify-center rounded-(--radius) hover:bg-accent"
          style={{ width: 20, height: 20, flexShrink: 0 }}
          onPress={(e: any) => {
            e?.stopPropagation?.();
            list.set_failure(props.target, null);
          }}
        >
          <Icon path={icon("circle-alert")} color={look().palette.bad} style={{ width: 13, height: 13, pointerEvents: "none" }} />
        </View>
      ) : null}
    </>
  );
}

function CollapsedSummary(props: { status: string; count: number; selected?: boolean }) {
  const look = () => theme();
  return (
    <View class="flex-row items-center" style={{ gap: 6, flexShrink: 0 }}>
      {props.status !== "idle" ? (
        <View
          style={{
            width: 6,
            height: 6,
            borderRadius: 3,
            backgroundColor: dotColor(look().palette, props.status),
          }}
        />
      ) : null}
      {props.count > 0 ? (
        <Text
          style={{
            color: rowFaint(look().palette, !!props.selected),
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
          }}
        >
          {String(props.count)}
        </Text>
      ) : null}
    </View>
  );
}

function InlineRenameInput(props: {
  rename: () => RenameState | null;
  onChange: (value: string) => void;
  onCommit: () => void;
  onCancel: () => void;
}) {
  const look = () => theme();
  return (
    <View class="flex-1 min-w-0" onPress={(e: any) => e?.stopPropagation?.()}>
      <Input
        testID="inline-rename-input"
        autoFocus={true}
        value={props.rename()?.value ?? ""}
        onChange={({ value }: { value: string }) => props.onChange(value)}
        onSubmit={() => props.onCommit()}
        onBlur={() => props.onCommit()}
        onKeyPress={(e: any) => {
          if (e?.key === "Escape") props.onCancel();
        }}
        onKeyDown={(e: any) => {
          if (e?.key === "Escape") props.onCancel();
        }}
        class="rounded-(--radius) border border-input bg-card px-1.5 text-foreground"
        style={{
          height: 22,
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
        }}
      />
    </View>
  );
}

/** 单行会话：支持跨 VM 头像插槽、行内改名、右键菜单（含外部插件贡献项）、拖拽排序。 */
function SessionRow(props: {
  row: () => any;
  level: number;
  avatarPlugin: () => string | null;
  hoveredKey: () => string | null;
  setHoveredKey: (key: string | null) => void;
  rename: () => RenameState | null;
  startRename: (target: RenameState["target"], original: string) => void;
  updateRename: (value: string) => void;
  commitRename: () => void;
  cancelRename: () => void;
  ask: (request: ConfirmRequest) => void;
  drag: () => DragState | null;
  beginDrag: (kind: "session", key: string, payload: any, y: number) => void;
  hoverDrag: (kind: "session", key: string, payload: any, y: number) => void;
  endDrag: (kind: "session", targetPayload: any, before: boolean) => void;
}) {
  const row = props.row;
  const look = () => theme();
  const id = () => session_flip(row().key);
  const selected = () => {
    tick();
    const active = selected_session(list.selection);
    return active != null && same_conversation(active, row().key);
  };
  const hovered = () => props.hoveredKey() === id();
  const renaming = () => isRenamingTarget(props.rename(), { kind: "conversation", key: row().key });
  const contributed = signal<ContributedMenuItem[]>([]);
  const dragDrop = () => {
    const d = props.drag();
    if (!d || !d.active || d.kind !== "session" || d.overKey !== id() || d.key === id()) return null;
    return d.before ? "top" : "bottom";
  };

  const loadContributed = () => {
    const current = row();
    fetchContextMenuItems(SESSION_MENU, current.key.machine, {
      machine: current.key.machine,
      conversation: current.key.id,
      session_id: current.session_id,
    }).then((items) => contributed.set(items));
  };

  const triggerClose = () => {
    const current = row();
    actions.close_session(list, props.ask, current.key, current.title, current.status);
  };

  const triggerOpen = () => {
    if (renaming()) return;
    const current = row();
    list.report_row_failure(
      { kind: "conversation", key: current.key },
      t("action.open_session"),
      list.open_conversation(current.key),
    );
  };

  const quiet = () => row().unavailable != null;
  const unread = () => row_tally(row()).unread > 0;
  const hasAvatar = computed(() => props.avatarPlugin() != null);
  const avatarId = computed(() => props.avatarPlugin() ?? "");
  const initialKey = row().key;
  const info = () => rowMeta(row(), selected());

  return (
    <ContextMenu
      popupClass={POPUP_CLASS}
      popupStyle={{ minWidth: 184 }}
      onOpenChange={({ open }) => {
        if (open) loadContributed();
      }}
      trigger={
        <View
          role="button"
          testID={`session-row:${row().key.id}`}
          onPress={(e: any) => {
            if (e?.clickCount === 2) {
              props.startRename({ kind: "conversation", key: row().key }, row().title);
              return;
            }
            triggerOpen();
          }}
          onPressDown={(e: any) => {
            if (row().group?.kind === "project") {
              props.beginDrag("session", id(), row(), e?.pagePosition?.y ?? 0);
            }
          }}
          onMouseEnter={() => props.setHoveredKey(id())}
          onMouseMove={(e: any) => {
            const d = props.drag();
            if (d && d.kind === "session") {
              props.hoverDrag("session", id(), row(), e?.pagePosition?.y ?? 0);
            }
          }}
          onMouseLeave={() => {
            if (props.hoveredKey() === id()) props.setHoveredKey(null);
          }}
          onMouseUp={() => {
            const d = props.drag();
            if (d && d.active && d.kind === "session") {
              props.endDrag("session", row(), d.before);
            }
          }}
          class={`relative flex-row items-center ${
            selected() ? "bg-sidebar-accent" : "hover:bg-accent active:bg-sidebar-accent"
          }`}
          style={{
            height: hasAvatar() ? AVATAR_ROW : ROW_MD,
            paddingLeft: indent(props.level),
            paddingRight: space(1),
            gap: space(2),
            borderRadius: look().radius.row,
            cursor: "default",
          }}
        >
          {dragDrop() === "top" ? (
            <View
              style={{
                position: "absolute",
                top: 0,
                left: 6,
                right: 6,
                height: 2,
                borderRadius: 1,
                backgroundColor: look().palette.link,
                pointerEvents: "none",
              }}
            />
          ) : null}
          {dragDrop() === "bottom" ? (
            <View
              style={{
                position: "absolute",
                bottom: 0,
                left: 6,
                right: 6,
                height: 2,
                borderRadius: 1,
                backgroundColor: look().palette.link,
                pointerEvents: "none",
              }}
            />
          ) : null}

          {hasAvatar() ? (
            <View style={{ position: "relative", width: AVATAR, height: AVATAR, flexShrink: 0 }}>
              <Slot
                id={avatarId()}
                args={{ machine: initialKey.machine, conversation: initialKey.id }}
                style={{ width: AVATAR, height: AVATAR }}
              />
              <View
                style={{
                  position: "absolute",
                  right: -2,
                  bottom: -1,
                  width: BADGE,
                  height: BADGE,
                  borderRadius: BADGE / 2,
                  alignItems: "center",
                  justifyContent: "center",
                  backgroundColor: look().palette.surface,
                  pointerEvents: "none",
                }}
              >
                <Icon
                  path={iconPath(row().icon)}
                  color={row().status === "idle" ? look().palette.text : dotColor(look().palette, row().status)}
                  style={{ width: 10, height: 10, pointerEvents: "none" }}
                />
              </View>
            </View>
          ) : (
            <View style={{ width: LEAD, height: LEAD, alignItems: "center", justifyContent: "center", flexShrink: 0 }}>
              <Icon
                path={iconPath(row().icon)}
                color={
                  row().status === "idle"
                    ? rowMuted(look().palette, selected())
                    : dotColor(look().palette, row().status)
                }
                style={{ width: LEAD, height: LEAD, pointerEvents: "none" }}
              />
            </View>
          )}

          {renaming() ? (
            <InlineRenameInput
              rename={props.rename}
              onChange={props.updateRename}
              onCommit={props.commitRename}
              onCancel={props.cancelRename}
            />
          ) : (
            <View class="flex-1 min-w-0 flex-row items-center" style={{ gap: space(1.5) }}>
              <Text
                class="flex-1 min-w-0"
                style={{
                  color: quiet() ? look().palette.muted : look().palette.text,
                  fontSize: look().type.ui.size,
                  lineHeight: look().type.ui.line_height,
                  fontWeight:
                    selected() || unread() ? look().type.ui_medium.weight : look().type.ui.weight,
                  numberOfLines: 1,
                }}
              >
                {row().title}
              </Text>
              {row().pinned ? (
                <Icon
                  path={icon("pin")}
                  color={look().palette.faint}
                  style={{ width: 11, height: 11, flexShrink: 0, pointerEvents: "none" }}
                />
              ) : null}
              {row().machine_name != null ? (
                <Text
                  style={{
                    color: rowMuted(look().palette, selected()),
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    numberOfLines: 1,
                    flexShrink: 0,
                  }}
                >
                  {row().machine_name}
                </Text>
              ) : null}
              {row().origin != null ? (
                <View class="flex-row items-center" style={{ gap: 4, flexShrink: 0 }}>
                  {row().origin.workset ? (
                    <Text
                      style={{
                        color: rowMuted(look().palette, selected()),
                        fontSize: look().type.text_xsmall.size,
                        lineHeight: look().type.text_xsmall.line_height,
                        numberOfLines: 1,
                      }}
                    >
                      {row().origin.workset.task}
                    </Text>
                  ) : null}
                  <Text
                    style={{
                      color: rowFaint(look().palette, selected()),
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      numberOfLines: 1,
                    }}
                  >
                    {row().origin.project}
                  </Text>
                </View>
              ) : null}
            </View>
          )}

          <FailureMark target={{ kind: "conversation", key: row().key }} />

          <View class="flex-row items-center justify-end" style={{ minWidth: 20, height: 20, flexShrink: 0 }}>
            {hovered() && !renaming() ? (
              <View
                role="button"
                accessibilityLabel={t("close.confirm")}
                testID={`close-session:${row().key.id}`}
                class="items-center justify-center rounded-(--radius) hover:bg-accent"
                style={{ width: 20, height: 20 }}
                onPress={(e: any) => {
                  e?.stopPropagation?.();
                  triggerClose();
                }}
              >
                <Icon path={icon("x")} color={look().palette.muted} style={{ width: 12, height: 12, pointerEvents: "none" }} />
              </View>
            ) : (
              <View class="flex-row items-center" style={{ gap: 4 }}>
                {info().unread ? (
                  <View style={{ width: 6, height: 6, borderRadius: 3, backgroundColor: look().palette.link }} />
                ) : null}
                <Text
                  style={{
                    color: info().color,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    numberOfLines: 1,
                  }}
                >
                  {info().text}
                </Text>
              </View>
            )}
          </View>
        </View>
      }
    >
      <MenuEntry
        label={row().pinned ? t("menu.unpin") : t("menu.pin")}
        iconPath={icon(row().pinned ? "pin-off" : "pin")}
        onSelect={() => actions.pin_session(list, row().key, row().group, !row().pinned)}
      />
      <MenuEntry
        label={t("menu.rename")}
        iconPath={icon("pencil")}
        onSelect={() => props.startRename({ kind: "conversation", key: row().key }, row().title)}
      />
      {row().session_id != null ? (
        <MenuEntry
          label={t("menu.copy_session_id")}
          iconPath={icon("copy")}
          onSelect={() => {
            const copyText = list.session_id_to_copy(row().key.machine, row().session_id);
            clipboardWrite(copyText).catch(() => {});
          }}
        />
      ) : null}
      <ContributedMenuItems
        items={contributed()}
        onFail={(command, err) =>
          list.fail_row(
            { kind: "conversation", key: row().key },
            t("action.run_command", { command }),
            message(err),
          )
        }
      />
      <MenuDivider />
      <MenuEntry
        label={t("archive.confirm")}
        iconPath={icon("archive")}
        onSelect={() => actions.archive_session(list, props.ask, row().key, row().title, row().status)}
      />
      <MenuEntry
        label={t("close.confirm")}
        iconPath={icon("x")}
        danger={true}
        onSelect={triggerClose}
      />
    </ContextMenu>
  );
}

/** 项目标题行 + 下面的工作树和会话。 */
function ProjectSection(props: {
  project: () => any;
  byMachine: boolean;
  avatarPlugin: () => string | null;
  hoveredKey: () => string | null;
  setHoveredKey: (key: string | null) => void;
  rename: () => RenameState | null;
  startRename: (target: RenameState["target"], original: string) => void;
  updateRename: (value: string) => void;
  commitRename: () => void;
  cancelRename: () => void;
  ask: (request: ConfirmRequest) => void;
  drag: () => DragState | null;
  beginDrag: (kind: "session" | "project", key: string, payload: any, y: number) => void;
  hoverDrag: (kind: "session" | "project", key: string, payload: any, y: number) => void;
  endDrag: (kind: "session" | "project", targetPayload: any, before: boolean) => void;
}) {
  const project = props.project;
  const look = () => theme();
  const key = () => project_flip(project().machine, project().root);
  const selected = () => {
    tick();
    const sel = list.selection;
    return sel.kind === "project" && sel.machine === project().machine && sel.root === project().root;
  };
  const hovered = () => props.hoveredKey() === key();
  const collapsible = () => project().items.length > 0;
  const collapsed = computed(() => project().collapsed);
  const contributed = signal<ContributedMenuItem[]>([]);
  const dragDrop = () => {
    const d = props.drag();
    if (!d || !d.active || d.kind !== "project" || d.overKey !== key() || d.key === key()) return null;
    return d.before ? "top" : "bottom";
  };

  const loadContributed = () => {
    const cur = project();
    if (!cur.root) return;
    fetchContextMenuItems(PROJECT_MENU, cur.machine, { machine: cur.machine, path: cur.root }).then((items) =>
      contributed.set(items),
    );
  };

  const toggleCollapse = () => {
    const cur = project();
    if (!collapsible()) {
      if (cur.root) {
        list.report_row_failure(
          { kind: "project", machine: cur.machine, root: cur.root },
          t("action.select_project"),
          list.select_project(cur.machine, cur.root),
        );
      }
      return;
    }
    list.set_collapsed({ kind: "project", machine: cur.machine, root: cur.root }, !cur.collapsed);
  };

  return (
    <View style={{ gap: 2 }}>
      <ContextMenu
        popupClass={POPUP_CLASS}
        popupStyle={{ minWidth: 184 }}
        onOpenChange={({ open }) => {
          if (open) loadContributed();
        }}
        trigger={
          <View
            role="button"
            testID={`project-row:${project().root}`}
            onPress={toggleCollapse}
            onPressDown={(e: any) => props.beginDrag("project", key(), project(), e?.pagePosition?.y ?? 0)}
            onMouseEnter={() => props.setHoveredKey(key())}
            onMouseMove={(e: any) => {
              const d = props.drag();
              if (d && d.kind === "project") {
                props.hoverDrag("project", key(), project(), e?.pagePosition?.y ?? 0);
              }
            }}
            onMouseLeave={() => {
              if (props.hoveredKey() === key()) props.setHoveredKey(null);
            }}
            onMouseUp={() => {
              const d = props.drag();
              if (d && d.active && d.kind === "project") {
                props.endDrag("project", project(), d.before);
              }
            }}
            class={`relative flex-row items-center ${
              selected() ? "bg-sidebar-accent" : "hover:bg-accent"
            }`}
            style={{
              height: ROW_MD,
              paddingLeft: space(2),
              paddingRight: space(1),
              gap: space(2),
              borderRadius: look().radius.row,
              cursor: "default",
            }}
          >
            {dragDrop() === "top" ? (
              <View
                style={{
                  position: "absolute",
                  top: 0,
                  left: 6,
                  right: 6,
                  height: 2,
                  borderRadius: 1,
                  backgroundColor: look().palette.link,
                  pointerEvents: "none",
                }}
              />
            ) : null}
            {dragDrop() === "bottom" ? (
              <View
                style={{
                  position: "absolute",
                  bottom: 0,
                  left: 6,
                  right: 6,
                  height: 2,
                  borderRadius: 1,
                  backgroundColor: look().palette.link,
                  pointerEvents: "none",
                }}
              />
            ) : null}

            <View style={{ width: LEAD, height: LEAD, alignItems: "center", justifyContent: "center", flexShrink: 0 }}>
              <Icon
                path={
                  collapsible() && (hovered() || project().collapsed)
                    ? icon(project().collapsed ? "chevron-right" : "chevron-down")
                    : icon(project().root ? "folder" : "folder-open")
                }
                color={look().palette.muted}
                style={{ width: 14, height: 14, pointerEvents: "none" }}
              />
            </View>

            <View class="flex-1 min-w-0 flex-row items-center" style={{ gap: space(1.5) }}>
              <Text
                class="min-w-0"
                style={{
                  color: project().link?.unavailable ? look().palette.muted : look().palette.text,
                  fontSize: look().type.ui.size,
                  lineHeight: look().type.ui.line_height,
                  fontWeight: look().type.ui_medium.weight,
                  numberOfLines: 1,
                  flexShrink: 1,
                }}
              >
                {project().name}
              </Text>
              {project().branch ? (
                <Text
                  style={{
                    color: rowFaint(look().palette, selected()),
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    numberOfLines: 1,
                    flexShrink: 1,
                  }}
                >
                  {project().branch}
                </Text>
              ) : null}
              {project().pinned ? (
                <Icon path={icon("pin")} color={look().palette.faint} style={{ width: 11, height: 11, flexShrink: 0, pointerEvents: "none" }} />
              ) : null}
              {!props.byMachine && project().machine_name != null ? (
                <Text
                  style={{
                    color: rowMuted(look().palette, selected()),
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    numberOfLines: 1,
                    flexShrink: 0,
                  }}
                >
                  {project().machine_name}
                </Text>
              ) : null}
            </View>

            <FailureMark target={{ kind: "project", machine: project().machine, root: project().root }} />

            {hovered() ? (
              <View class="flex-row items-center" style={{ gap: 2, flexShrink: 0 }}>
                <View
                  role="button"
                  accessibilityLabel={t("menu.close_project")}
                  class="items-center justify-center rounded-(--radius) hover:bg-accent"
                  style={{ width: 20, height: 20 }}
                  onPress={(e: any) => {
                    e?.stopPropagation?.();
                    const cur = project();
                    actions.close_project(list, props.ask, cur.machine, cur.root, cur.name, cur.sessions);
                  }}
                >
                  <Icon path={icon("x")} color={look().palette.muted} style={{ width: 12, height: 12, pointerEvents: "none" }} />
                </View>
              </View>
            ) : project().collapsed ? (
              <CollapsedSummary status={project().status} count={project().sessions} selected={selected()} />
            ) : null}
          </View>
        }
      >
        <MenuEntry
          label={project().pinned ? t("menu.unpin") : t("menu.pin")}
          iconPath={icon(project().pinned ? "pin-off" : "pin")}
          onSelect={() => actions.pin_project(list, project().machine, project().root, !project().pinned)}
        />
        {project().root ? (
          <MenuEntry
            label={t("menu.copy_project_path")}
            iconPath={icon("copy")}
            onSelect={() => clipboardWrite(project().root).catch(() => {})}
          />
        ) : null}
        {project().branch ? (
          <MenuEntry
            label={t("menu.copy_branch")}
            iconPath={icon("git-branch")}
            onSelect={() => clipboardWrite(project().branch).catch(() => {})}
          />
        ) : null}
        <ContributedMenuItems
          items={contributed()}
          onFail={(command, err) =>
            list.fail_row(
              { kind: "project", machine: project().machine, root: project().root },
              t("action.run_command", { command }),
              message(err),
            )
          }
        />
        <MenuDivider />
        <MenuEntry
          label={project().opened ? t("menu.close_project") : t("menu.close_group")}
          iconPath={icon("x")}
          danger={true}
          onSelect={() => {
            const cur = project();
            actions.close_project(list, props.ask, cur.machine, cur.root, cur.name, cur.sessions);
          }}
        />
      </ContextMenu>

      {!collapsed() ? (
        <Each
          of={project().items}
          key={(item: any) =>
            item.kind === "session"
              ? session_flip(item.row.key)
              : workset_flip(item.workset.machine, item.workset.id)
          }
        >
          {(item: () => any) =>
            item().kind === "session" ? (
              <SessionRow
                row={() => item().row}
                level={1}
                avatarPlugin={props.avatarPlugin}
                hoveredKey={props.hoveredKey}
                setHoveredKey={props.setHoveredKey}
                rename={props.rename}
                startRename={props.startRename}
                updateRename={props.updateRename}
                commitRename={props.commitRename}
                cancelRename={props.cancelRename}
                ask={props.ask}
                drag={props.drag}
                beginDrag={props.beginDrag}
                hoverDrag={props.hoverDrag}
                endDrag={props.endDrag}
              />
            ) : (
              <WorksetSection
                workset={() => item().workset}
                avatarPlugin={props.avatarPlugin}
                hoveredKey={props.hoveredKey}
                setHoveredKey={props.setHoveredKey}
                rename={props.rename}
                startRename={props.startRename}
                updateRename={props.updateRename}
                commitRename={props.commitRename}
                cancelRename={props.cancelRename}
                ask={props.ask}
                drag={props.drag}
                beginDrag={props.beginDrag}
                hoverDrag={props.hoverDrag}
                endDrag={props.endDrag}
              />
            )
          }
        </Each>
      ) : null}
    </View>
  );
}

/** 工作树子分组及其下面的会话。 */
function WorksetSection(props: {
  workset: () => any;
  avatarPlugin: () => string | null;
  hoveredKey: () => string | null;
  setHoveredKey: (key: string | null) => void;
  rename: () => RenameState | null;
  startRename: (target: RenameState["target"], original: string) => void;
  updateRename: (value: string) => void;
  commitRename: () => void;
  cancelRename: () => void;
  ask: (request: ConfirmRequest) => void;
  drag: () => DragState | null;
  beginDrag: (kind: "session", key: string, payload: any, y: number) => void;
  hoverDrag: (kind: "session", key: string, payload: any, y: number) => void;
  endDrag: (kind: "session", targetPayload: any, before: boolean) => void;
}) {
  const workset = props.workset;
  const look = () => theme();
  const key = () => workset_flip(workset().machine, workset().id);
  const hovered = () => props.hoveredKey() === key();
  const collapsed = computed(() => workset().collapsed);
  const renaming = () =>
    isRenamingTarget(props.rename(), { kind: "workset", machine: workset().machine, id: workset().id });

  return (
    <View style={{ gap: 2 }}>
      <ContextMenu
        popupClass={POPUP_CLASS}
        popupStyle={{ minWidth: 184 }}
        trigger={
          <View
            role="button"
            onPress={() => {
              if (renaming()) return;
              list.set_collapsed({ kind: "workset", machine: workset().machine, id: workset().id }, !workset().collapsed);
            }}
            onMouseEnter={() => props.setHoveredKey(key())}
            onMouseLeave={() => {
              if (props.hoveredKey() === key()) props.setHoveredKey(null);
            }}
            class="flex-row items-center hover:bg-accent"
            style={{
              height: ROW_MD,
              paddingLeft: indent(1),
              paddingRight: space(1),
              gap: space(2),
              borderRadius: look().radius.row,
              cursor: "default",
            }}
          >
            <View style={{ width: LEAD, height: LEAD, alignItems: "center", justifyContent: "center", flexShrink: 0 }}>
              <Icon
                path={
                  workset().sessions.length > 0 && (hovered() || workset().collapsed)
                    ? icon(workset().collapsed ? "chevron-right" : "chevron-down")
                    : icon("git-branch")
                }
                color={workset().merge === "merged" ? look().palette.ok : look().palette.muted}
                style={{ width: 13, height: 13, pointerEvents: "none" }}
              />
            </View>

            {renaming() ? (
              <InlineRenameInput
                rename={props.rename}
                onChange={props.updateRename}
                onCommit={props.commitRename}
                onCancel={props.cancelRename}
              />
            ) : (
              <View class="flex-1 min-w-0 flex-row items-center" style={{ gap: space(1.5) }}>
                <Text
                  class="flex-1 min-w-0"
                  style={{
                    color: look().palette.text,
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    numberOfLines: 1,
                  }}
                >
                  {workset().task}
                </Text>
                {workset().pinned ? (
                  <Icon path={icon("pin")} color={look().palette.faint} style={{ width: 11, height: 11, flexShrink: 0, pointerEvents: "none" }} />
                ) : null}
              </View>
            )}

            <FailureMark target={{ kind: "workset", machine: workset().machine, id: workset().id }} />

            {workset().collapsed ? (
              <CollapsedSummary status={workset().status} count={workset().sessions.length} />
            ) : null}
          </View>
        }
      >
        <MenuEntry
          label={workset().pinned ? t("menu.unpin") : t("menu.pin")}
          iconPath={icon(workset().pinned ? "pin-off" : "pin")}
          onSelect={() => actions.pin_workset(list, workset().machine, workset().id, !workset().pinned)}
        />
        <MenuEntry
          label={t("menu.rename_workset")}
          iconPath={icon("pencil")}
          onSelect={() =>
            props.startRename({ kind: "workset", machine: workset().machine, id: workset().id }, workset().task)
          }
        />
        <MenuEntry
          label={workset().checking ? t("menu.checking_merge") : t("menu.check_merge")}
          iconPath={icon("git-merge")}
          disabled={workset().checking}
          onSelect={() => actions.check_workset(list, workset().machine, workset().id)}
        />
        <MenuDivider />
        <MenuEntry
          label={t("menu.finish_workset")}
          iconPath={icon("x")}
          danger={true}
          onSelect={() => actions.finish_workset(list, props.ask, workset().machine, workset().id, workset().task)}
        />
      </ContextMenu>

      {!collapsed() ? (
        <Each of={workset().sessions} key={(row: any) => session_flip(row.key)}>
          {(row: () => any) => (
            <SessionRow
              row={row}
              level={2}
              avatarPlugin={props.avatarPlugin}
              hoveredKey={props.hoveredKey}
              setHoveredKey={props.setHoveredKey}
              rename={props.rename}
              startRename={props.startRename}
              updateRename={props.updateRename}
              commitRename={props.commitRename}
              cancelRename={props.cancelRename}
              ask={props.ask}
              drag={props.drag}
              beginDrag={props.beginDrag}
              hoverDrag={props.hoverDrag}
              endDrag={props.endDrag}
            />
          )}
        </Each>
      ) : null}
    </View>
  );
}

/** 列表主视图（默认导出，由宿主按跨 VM 插槽挂载）。 */
export default function SessionListView(props: { args?: { avatarPlugin?: string } }) {
  const searching = signal(false);
  const rename = signal<RenameState | null>(null);
  const confirm = signal<ConfirmRequest | null>(null);
  const hoveredKey = signal<string | null>(null);
  const drag = signal<DragState | null>(null);

  const look = () => theme();
  const sidebar = () => {
    tick();
    return list.sidebar;
  };
  const avatarPlugin = computed((): string | null => {
    tick();
    const filled = slotViews(SESSION_AVATAR);
    if (filled.length > 0) return pick_avatar(list.avatar, filled);
    const fallback = props.args?.avatarPlugin;
    return fallback ? pick_avatar(list.avatar, [fallback]) : null;
  });
  const hasConversations = computed(() => sidebar().conversations != null);
  const conversationsCollapsed = computed(() => sidebar().conversations?.collapsed ?? false);
  const byProject = computed(() => sidebar().grouping === "project");

  const ask = (request: ConfirmRequest) => confirm.set(request);

  const startRename = (target: RenameState["target"], original: string) => {
    rename.set({ target, original, value: original });
  };
  const updateRename = (value: string) => {
    const cur = rename();
    if (cur) rename.set({ ...cur, value });
  };
  const commitRename = () => {
    const cur = rename();
    if (!cur) return;
    rename.set(null);
    list.commit_rename(cur.target, cur.original, cur.value);
  };
  const cancelRename = () => rename.set(null);

  const beginDrag = (kind: "session" | "project", key: string, payload: any, y: number) => {
    drag.set({ kind, key, payload, startY: y, active: false, overKey: null, before: true });
  };
  const hoverDrag = (kind: "session" | "project", key: string, payload: any, y: number) => {
    const cur = drag();
    if (!cur || cur.kind !== kind) return;
    const active = cur.active || Math.abs(y - cur.startY) >= DRAG_THRESHOLD;
    const before = y < cur.startY;
    if (cur.active !== active || cur.overKey !== key || cur.before !== before) {
      drag.set({ ...cur, active, overKey: key, before });
    }
  };
  const endDrag = (kind: "session" | "project", targetPayload: any, before: boolean) => {
    const cur = drag();
    drag.set(null);
    if (!cur || !cur.active || cur.kind !== kind) return;
    if (kind === "project") {
      actions.drop_project(
        list,
        { machine: cur.payload.machine, root: cur.payload.root },
        { machine: targetPayload.machine, root: targetPayload.root },
        before,
      );
    } else if (kind === "session" && cur.payload.group?.kind === "project") {
      actions.drop_session(list, cur.payload.key, targetPayload.key, cur.payload.group, before);
    }
  };

  const visibleProjectsOf = (area: any) => {
    const sb = sidebar();
    const sel = list.selection;
    return sb.area_projects(area).filter((project: any) => {
      const isSelected =
        sel.kind === "project" && sel.machine === project.machine && sel.root === project.root;
      return project_visible(sb.hide_empty, project.items.length > 0, project.pinned, isSelected);
    });
  };

  const hiddenCountOf = (area: any) => {
    const sb = sidebar();
    if (sb.grouping !== "project" || !sb.hide_empty) return 0;
    return sb.area_projects(area).length - visibleProjectsOf(area).length;
  };

  const addProjectButton = () => {
    const sb = sidebar();
    if (!sb.has_remote()) {
      return (
        <View
          role="button"
          accessibilityLabel={t("list.add_project")}
          class="items-center justify-center rounded-(--radius) hover:bg-accent"
          style={{ width: 24, height: 24 }}
          onPress={() => actions.open_project(list, null)}
        >
          <Icon path={icon("plus")} color={look().palette.muted} style={{ width: 14, height: 14, pointerEvents: "none" }} />
        </View>
      );
    }
    return (
      <Menu
        placement="bottom"
        align="end"
        popupClass={POPUP_CLASS}
        popupStyle={{ minWidth: 176 }}
        trigger={
          <View
            role="button"
            accessibilityLabel={t("list.add_project")}
            class="items-center justify-center rounded-(--radius) hover:bg-accent"
            style={{ width: 24, height: 24 }}
          >
            <Icon path={icon("plus")} color={look().palette.muted} style={{ width: 14, height: 14, pointerEvents: "none" }} />
          </View>
        }
      >
        <MenuSectionTitle text={t("menu.open_on_machine")} />
        <Each of={sb.machines} key={(m: any) => machine_flip(m.machine)}>
          {(m: () => any) => (
            <MenuEntry
              label={m().name}
              iconPath={m().os_icon}
              disabled={m().link?.unavailable != null}
              onSelect={() => actions.open_project(list, m().machine)}
            />
          )}
        </Each>
      </Menu>
    );
  };

  return (
    <View
      testID="session-list-root"
      style={{ flex: 1, minHeight: 0 }}
      onMouseUp={() => {
        if (drag()) drag.set(null);
      }}
      onFileDrop={(e: any) => {
        const uris = Array.isArray(e?.uris) ? e.uris : [];
        const paths = decodeFileUris(uris);
        if (paths.length > 0) {
          actions.open_dropped_paths(list, paths);
        }
      }}
      onKeyDown={(e: any) => {
        if (e?.key === "Escape") {
          if (confirm()) {
            confirm.set(null);
            return;
          }
          if (rename()) {
            cancelRename();
            return;
          }
          if (searching()) {
            searching.set(false);
            list.set_project_filter("");
          }
          return;
        }
        if ((e?.modifiers?.meta || e?.modifiers?.ctrl) && e?.key === "ArrowUp") {
          list.step_session(false).catch(() => {});
        } else if ((e?.modifiers?.meta || e?.modifiers?.ctrl) && e?.key === "ArrowDown") {
          list.step_session(true).catch(() => {});
        }
      }}
    >
      <Scroll style={{ flex: 1 }}>
        <View style={{ paddingHorizontal: space(2), paddingBottom: space(2), gap: space(1) }}>
          {/* 「对话」段 */}
          {hasConversations() ? (
            <View style={{ gap: 2, paddingBottom: space(1) }}>
              <View
                role="button"
                class="flex-row items-center hover:bg-accent"
                style={{
                  height: 28,
                  paddingHorizontal: space(2),
                  gap: space(1.5),
                  borderRadius: look().radius.row,
                  cursor: "default",
                }}
                onPress={() =>
                  list.set_collapsed({ kind: "conversations" }, !sidebar().conversations.collapsed)
                }
                onMouseEnter={() => hoveredKey.set(CONVERSATIONS_KEY)}
                onMouseLeave={() => {
                  if (hoveredKey() === CONVERSATIONS_KEY) hoveredKey.set(null);
                }}
              >
                <Icon
                  path={icon(conversationsCollapsed() ? "chevron-right" : "chevron-down")}
                  color={look().palette.muted}
                  style={{ width: 12, height: 12, pointerEvents: "none" }}
                />
                <Text
                  class="flex-1 min-w-0"
                  style={{
                    color: look().palette.muted,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    fontWeight: look().type.text_xsmall_medium.weight,
                  }}
                >
                  {t("kind.conversation_section")}
                </Text>
                {conversationsCollapsed() ? (
                  <CollapsedSummary
                    status={sidebar().conversations.status}
                    count={sidebar().conversations.rows.length}
                  />
                ) : null}
              </View>
              {!conversationsCollapsed() ? (
                <Each of={sidebar().conversations.rows} key={(row: any) => session_flip(row.key)}>
                  {(row: () => any) => (
                    <SessionRow
                      row={row}
                      level={0}
                      avatarPlugin={avatarPlugin}
                      hoveredKey={hoveredKey}
                      setHoveredKey={(k) => hoveredKey.set(k)}
                      rename={rename}
                      startRename={startRename}
                      updateRename={updateRename}
                      commitRename={commitRename}
                      cancelRename={cancelRename}
                      ask={ask}
                      drag={drag}
                      beginDrag={beginDrag}
                      hoverDrag={hoverDrag}
                      endDrag={endDrag}
                    />
                  )}
                </Each>
              ) : null}
            </View>
          ) : null}

          {/* 「项目」标题栏（兼项目搜索 + 分组菜单 + 添加项目） */}
          <View class="flex-row items-center" style={{ height: 28, paddingLeft: space(2), gap: space(1) }}>
            {searching() ? (
              <View class="flex-1 min-w-0">
                <Input
                  testID="project-search-input"
                  autoFocus={true}
                  placeholder={t("list.search_projects")}
                  value={list.project_filter}
                  onChange={({ value }: { value: string }) => list.set_project_filter(value)}
                  onBlur={() => {
                    if (list.project_filter.trim() === "") searching.set(false);
                  }}
                  onKeyPress={(e: any) => {
                    if (e?.key === "Escape") {
                      searching.set(false);
                      list.set_project_filter("");
                    }
                  }}
                  onKeyDown={(e: any) => {
                    if (e?.key === "Escape") {
                      searching.set(false);
                      list.set_project_filter("");
                    }
                  }}
                  class="rounded-(--radius) border border-input bg-card px-1.5 text-foreground"
                  style={{
                    height: 22,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                  }}
                />
              </View>
            ) : (
              <View
                role="button"
                testID="search-projects-trigger"
                class="flex-1 min-w-0 flex-row items-center rounded-(--radius) hover:bg-accent"
                style={{ height: 24, paddingHorizontal: space(1), gap: space(1.5), cursor: "text" }}
                onPress={() => searching.set(true)}
              >
                <Text
                  class="flex-1 min-w-0"
                  style={{
                    color: look().palette.muted,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    fontWeight: look().type.text_xsmall_medium.weight,
                    numberOfLines: 1,
                  }}
                >
                  {t("grouping.project")}
                </Text>
                <Icon
                  path={icon("search")}
                  color={look().palette.faint}
                  style={{ width: 12, height: 12, flexShrink: 0, pointerEvents: "none" }}
                />
              </View>
            )}
            <GroupingMenu />
            {addProjectButton()}
          </View>

          {/* 区域：按机器或整份 */}
          <Each
            of={sidebar().areas}
            key={(area: any) => (area.machine === undefined ? "all" : machine_flip(area.machine))}
          >
            {(area: () => any) => {
              const machineInfo = () =>
                area().machine === undefined
                  ? null
                  : sidebar().machines.find((m: any) => m.machine === area().machine) ?? null;
              const areaCollapsed = computed(() => area().collapsed);
              return (
                <View style={{ gap: 2 }}>
                  {machineInfo() ? (
                    <View
                      role="button"
                      class="flex-row items-center hover:bg-accent"
                      style={{
                        height: 28,
                        paddingHorizontal: space(2),
                        gap: space(2),
                        borderRadius: look().radius.row,
                        cursor: "default",
                      }}
                      onPress={() => list.set_machine_collapsed(area().machine, !area().collapsed)}
                    >
                      <Icon
                        path={machineInfo()!.os_icon}
                        color={lightColor(look().palette, machineInfo()!.link.light)}
                        style={{ width: 14, height: 14, pointerEvents: "none" }}
                      />
                      <Text
                        class="flex-1 min-w-0"
                        style={{
                          color: look().palette.text,
                          fontSize: look().type.ui.size,
                          lineHeight: look().type.ui.line_height,
                          fontWeight: look().type.ui_medium.weight,
                          numberOfLines: 1,
                        }}
                      >
                        {machineInfo()!.name}
                      </Text>
                      {areaCollapsed() ? (
                        <CollapsedSummary status={area().status} count={area().sessions} />
                      ) : null}
                    </View>
                  ) : null}

                  {!areaCollapsed() ? (
                    byProject() ? (
                      <>
                        <Each
                          of={visibleProjectsOf(area())}
                          key={(project: any) => project_flip(project.machine, project.root)}
                        >
                          {(project: () => any) => (
                            <ProjectSection
                              project={project}
                              byMachine={sidebar().by_machine}
                              avatarPlugin={avatarPlugin}
                              hoveredKey={hoveredKey}
                              setHoveredKey={(k) => hoveredKey.set(k)}
                              rename={rename}
                              startRename={startRename}
                              updateRename={updateRename}
                              commitRename={commitRename}
                              cancelRename={cancelRename}
                              ask={ask}
                              drag={drag}
                              beginDrag={beginDrag}
                              hoverDrag={hoverDrag}
                              endDrag={endDrag}
                            />
                          )}
                        </Each>

                        {hiddenCountOf(area()) > 0 ? (
                          <View
                            role="button"
                            class="flex-row items-center rounded-(--radius) hover:bg-accent"
                            style={{ height: 28, paddingHorizontal: space(2), cursor: "default" }}
                            onPress={() => list.set_hide_empty_projects(false)}
                          >
                            <Text
                              style={{
                                color: look().palette.faint,
                                fontSize: look().type.text_xsmall.size,
                                lineHeight: look().type.text_xsmall.line_height,
                              }}
                            >
                              {hiddenCountOf(area()) === 1
                                ? t("list.hidden_projects.one")
                                : t("list.hidden_projects.other", { count: hiddenCountOf(area()) })}
                            </Text>
                          </View>
                        ) : null}

                        {visibleProjectsOf(area()).length === 0 && hiddenCountOf(area()) === 0 ? (
                          <View style={{ paddingHorizontal: space(2), paddingVertical: space(3) }}>
                            <Text
                              style={{
                                color: look().palette.faint,
                                fontSize: look().type.text_xsmall.size,
                                lineHeight: look().type.text_xsmall.line_height,
                              }}
                            >
                              {sidebar().filtering ? t("list.no_matching_projects") : t("list.no_projects")}
                            </Text>
                          </View>
                        ) : null}
                      </>
                    ) : (
                      <>
                        <Each of={area().buckets} key={(group: any) => bucket_flip(group.key)}>
                          {(group: () => any) => {
                            const bucketCollapsed = computed(() => group().collapsed);
                            return (
                            <View style={{ gap: 2 }}>
                              <View
                                role="button"
                                class="flex-row items-center hover:bg-accent"
                                style={{
                                  height: 28,
                                  paddingHorizontal: space(2),
                                  gap: space(1.5),
                                  borderRadius: look().radius.row,
                                  cursor: "default",
                                }}
                                onPress={() =>
                                  list.set_bucket_collapsed(bucket_flip(group().key), !group().collapsed)
                                }
                              >
                                <Icon
                                  path={icon(bucketCollapsed() ? "chevron-right" : "chevron-down")}
                                  color={look().palette.muted}
                                  style={{ width: 12, height: 12, pointerEvents: "none" }}
                                />
                                <Text
                                  class="flex-1 min-w-0"
                                  style={{
                                    color: look().palette.muted,
                                    fontSize: look().type.text_xsmall.size,
                                    lineHeight: look().type.text_xsmall.line_height,
                                    fontWeight: look().type.text_xsmall_medium.weight,
                                  }}
                                >
                                  {bucket_label(group().key.bucket, t)}
                                </Text>
                                {bucketCollapsed() ? (
                                  <CollapsedSummary status={group().status} count={group().rows.length} />
                                ) : null}
                              </View>
                              {!bucketCollapsed() ? (
                                <Each of={group().rows} key={(row: any) => session_flip(row.key)}>
                                  {(row: () => any) => (
                                    <SessionRow
                                      row={row}
                                      level={1}
                                      avatarPlugin={avatarPlugin}
                                      hoveredKey={hoveredKey}
                                      setHoveredKey={(k) => hoveredKey.set(k)}
                                      rename={rename}
                                      startRename={startRename}
                                      updateRename={updateRename}
                                      commitRename={commitRename}
                                      cancelRename={cancelRename}
                                      ask={ask}
                                      drag={drag}
                                      beginDrag={beginDrag}
                                      hoverDrag={hoverDrag}
                                      endDrag={endDrag}
                                    />
                                  )}
                                </Each>
                              ) : null}
                            </View>
                            );
                          }}
                        </Each>
                        {area().buckets.length === 0 ? (
                          <View style={{ paddingHorizontal: space(2), paddingVertical: space(3) }}>
                            <Text
                              style={{
                                color: look().palette.faint,
                                fontSize: look().type.text_xsmall.size,
                                lineHeight: look().type.text_xsmall.line_height,
                              }}
                            >
                              {t("list.no_sessions")}
                            </Text>
                          </View>
                        ) : null}
                      </>
                    )
                  ) : null}
                </View>
              );
            }}
          </Each>
        </View>
      </Scroll>

      <ConfirmDialog request={confirm} onDismiss={() => confirm.set(null)} />
    </View>
  );
}
