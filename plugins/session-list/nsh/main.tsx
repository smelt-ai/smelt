// 会话列表插件：
// 复用 `plugins/session-list/` 全部纯 JS 逻辑与 SQLite 存储，导出：
// - `root`：承载新建会话面板与项目仓库管理对话框（挂在主壳 0×0 无头根层）；
// - `main`：侧栏会话列表视图（行首会话头像 `<Avatar>`（共享模块 `smelt/avatar`，智能体会话的眼睛是智能体图标）、分组折叠弹簧动画、悬停图标切换、
//   拖拽浮卡与落点线、新建会话「+」弹层、搜索、分组/机器菜单、可视区外提示胶囊、动效与外部拖入项目）；
// - `settings`：设置「通用 → 会话列表」行首头像选择。

import { cleanup, computed, createAnimatedValue, Easing, effect, Overlay, Scroll, signal, Text, View } from "nsh";
import { Input, Submenu } from "nsh/base";
import {
  fileUriToPath,
  SmeltButton,
  SmeltContextMenu,
  SmeltDisclosure,
  SmeltDisclosureChevron,
  SmeltIcon,
  SmeltMenu,
  SmeltWindowBar,
  viewShown,
} from "smelt/sdk";
import { Avatar } from "smelt/avatar";

import * as actions from "../actions.js";
import {
  LIST,
  MACHINE_MENU,
  PROJECT_MENU,
  SESSION_MENU,
} from "../contract.js";
import { bucket_label } from "../group.js";
import { machine_id, same_conversation, selected_session, show_avatar } from "../model.js";
import { ListMotion, WASH_END } from "../motion.js";
import { hint, tally_is_empty, tally_label } from "../offscreen.js";
import {
  bucket_flip,
  CONVERSATIONS_KEY,
  machine_flip,
  project_flip,
  project_visible,
  row_tally,
  session_flip,
  workset_flip,
} from "../sidebar.js";
import { fail_key } from "../state.js";
import { relative_time, span } from "../time.js";
import { icon, message } from "../util.js";
import {
  avatarActor,
  clipboardWrite,
  ContributedMenuItem,
  fetchContextMenuItems,
  hostAdapter,
  list,
  machineMenuVersion,
  registerFocusCallback,
  space,
  t,
  theme,
  tick,
} from "./bridge.ts";
import {
  ConfirmDialog,
  ConfirmRequest,
  ContributedMenuItems,
  fetchMachinesMenuGroups,
  GroupingMenu,
  MachineMenuGroup,
  MachinesMenuEntries,
  MENU_ITEM_CLASS,
  MenuDivider,
  MenuEntry,
  MenuSectionTitle,
  Tip,
} from "./menus.tsx";
import { NewSessionPopover, PaletteDialog } from "./new_session_view.tsx";
import { RepositoriesDialog } from "./repositories_view.tsx";
import { SessionListSettings } from "./settings_view.tsx";

const ROW_SM = 28;
const ROW_MD = 32;
const AVATAR = 36;
const AVATAR_ROW = 44;
const LEAD = 16;
const EDGE = 8;
const INDENT = 16;
const DRAG_THRESHOLD = 4;
const AUTOSCROLL_EDGE = 40;
/** 拖到边上时每毫秒滚多少：按两帧间隔算，高刷屏不会滚得更快。 */
const AUTOSCROLL_SPEED = 0.5;
const WASH_RISE_MS = 120;
const WASH_HOLD_MS = 360;
const WASH_PEAK_OPACITY = 0.1;

interface RenameState {
  target:
    | { kind: "conversation"; key: { machine: string | null; id: string } }
    | { kind: "workset"; machine: string | null; id: string };
  original: string;
  value: string;
}

interface DragState {
  kind: "session" | "project";
  dragKind: string;
  key: string;
  payload: any;
  previewTitle: string;
  previewSub: string | null;
  previewIcon: string;
  previewIconColor: string;
  previewHeight: number;
  previewWidth: number;
  previewLeft: number;
  grabOffsetY: number;
  startY: number;
  pointerY: number;
  active: boolean;
  overKey: string | null;
  overPayload: any;
  before: boolean;
  lastTargetY: number;
}

interface MotionAccess {
  washSerial: (key: string) => number | null;
  glowSerial: (key: string) => number | null;
}

/** 状态只有「要你」配色；在跑用正文色，比空闲的次要字亮一档，不另配彩色。 */
function dotColor(palette: ReturnType<typeof theme>["palette"], status: string): string {
  if (status === "needs_you") return palette.need;
  if (status === "running") return palette.text;
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

function providerTip(rowIcon: any): string | null {
  return rowIcon?.kind === "provider" && rowIcon.label ? String(rowIcon.label) : null;
}

function indent(level: number): number {
  return EDGE + level * INDENT;
}

function projectDragKind(sidebar: any, project: any): string {
  return sidebar.by_machine ? `project:${machine_id(project.machine)}` : "project";
}

function sessionDragKind(sidebar: any, row: any): string | null {
  const group = row?.group;
  if (sidebar.grouping !== "project" || group?.kind !== "project") return null;
  return `session:${machine_id(group.machine)}:${group.root}:${group.workset ?? ""}`;
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
    // 在跑：计时用强调色，和空闲的「几分钟前」一眼分开；不另加转圈。
    return { text: span(Math.max(nowSec - row.turn_started_at, 0), t), color: palette.accent, unread };
  }
  return { text: relative_time(row.activity, nowSec, t), color: rowFaint(palette, selected), unread };
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

let lastConnectionsRefreshAt = 0;
function refreshConnectionsThrottled() {
  const now = Date.now();
  if (now - lastConnectionsRefreshAt < 2000) return;
  lastConnectionsRefreshAt = now;
  hostAdapter.run("smelt.app/refresh-connections", null).catch(() => {});
}

/** 分组行行首图标槽：平时画分组图标，悬停整行（group/row）时原位换成弹簧旋转箭头；不挪动文字。 */
function DisclosureSlot(props: {
  baseIcon: string;
  baseColor?: string;
  baseSize?: number;
  baseTooltip?: string | null;
  onBaseHover?: () => void;
  foldKey: string;
  collapsed: boolean;
  collapsible: boolean;
  opacity?: number;
  onToggle?: () => void;
}) {
  const look = () => theme();
  const baseSize = () => props.baseSize ?? 14;
  const baseColor = () => props.baseColor ?? look().palette.muted;
  const iconOpacity = () => props.opacity ?? 1;

  const content = (
    <View
      style={{
        position: "relative",
        width: LEAD,
        height: LEAD,
        alignItems: "center",
        justifyContent: "center",
        flexShrink: 0,
      }}
    >
      <View
        role={props.collapsible ? "image" : undefined}
        testID={props.collapsible ? `disclosure-icon:${props.foldKey}` : undefined}
        accessibilityLabel={props.collapsible ? props.baseIcon : undefined}
        class={
          props.collapsible
            ? "absolute inset-0 items-center justify-center opacity-100 visible group-hover/row:opacity-0 group-hover/row:invisible"
            : "absolute inset-0 items-center justify-center opacity-100"
        }
      >
        <SmeltIcon
          path={props.baseIcon}
          size={baseSize()}
          color={baseColor()}
          opacity={iconOpacity()}
        />
      </View>
      <If when={props.collapsible}>
        <View
          role={props.onToggle ? "button" : "image"}
          testID={`disclosure-chevron:${props.foldKey}`}
          accessibilityLabel={props.onToggle ? (props.collapsed ? "Expand" : "Collapse") : "icons/chevron-right.svg"}
          class="absolute inset-0 items-center justify-center opacity-0 invisible group-hover/row:opacity-100 group-hover/row:visible"
        >
          <SmeltDisclosureChevron
            open={!props.collapsed}
            size={14}
            color={look().palette.muted}
            opacity={iconOpacity()}
          />
        </View>
      </If>
    </View>
  );

  if (props.onToggle) {
    return (
      <View
        tooltip={props.baseTooltip ? <Tip text={props.baseTooltip} /> : undefined}
        onMouseEnter={props.onBaseHover}
        onPress={(e: any) => {
          if (!props.collapsible) return;
          e?.stopPropagation?.();
          props.onToggle?.();
        }}
        style={{
          width: LEAD,
          height: LEAD,
          alignItems: "center",
          justifyContent: "center",
          flexShrink: 0,
          cursor: "default",
        }}
      >
        {content}
      </View>
    );
  }

  return (
    <View
      tooltip={props.baseTooltip ? <Tip text={props.baseTooltip} /> : undefined}
      onMouseEnter={props.onBaseHover}
      style={{
        width: LEAD,
        height: LEAD,
        alignItems: "center",
        justifyContent: "center",
        flexShrink: 0,
      }}
    >
      {content}
    </View>
  );
}

/** 2px 拖拽落点指示线，落在目标行的上边或下边。 */
function DropLine(props: { edge: "top" | "bottom" | null }) {
  const look = () => theme();
  return (
    <If when={props.edge != null}>
      <View
        style={{
          position: "absolute",
          top: props.edge === "top" ? -1 : undefined,
          bottom: props.edge === "bottom" ? -1 : undefined,
          left: 4,
          right: 4,
          height: 2,
          borderRadius: 1,
          backgroundColor: look().palette.accent,
          pointerEvents: "none",
        }}
      />
    </If>
  );
}

/** 定位/新落进列表时铺一层底色淡入、停住、慢慢退干净（原生 UI 线程关键帧，无 JS 定时器）。 */
function WashLayer(props: { serial: () => number | null }) {
  const look = () => theme();
  const opacity = createAnimatedValue(0);
  let lastSeen = 0;

  effect(() => {
    const serial = props.serial();
    if (serial == null || serial <= 0 || serial === lastSeen) return;
    lastSeen = serial;
    void opacity.keyframes(
      [
        { offset: 0, value: 0, easing: Easing.inOut(Easing.quad) },
        { offset: WASH_RISE_MS / WASH_END, value: WASH_PEAK_OPACITY, easing: Easing.linear },
        { offset: WASH_HOLD_MS / WASH_END, value: WASH_PEAK_OPACITY, easing: Easing.inOut(Easing.quad) },
        { offset: 1, value: 0 },
      ],
      { duration: WASH_END },
    );
  });

  cleanup(() => {
    opacity.stop();
  });

  return (
    <View
      style={{
        position: "absolute",
        top: 0,
        right: 0,
        bottom: 0,
        left: 0,
        borderRadius: look().radius.row,
        backgroundColor: look().palette.text,
        opacity,
        pointerEvents: "none",
      }}
    />
  );
}

function FailureMark(props: { target: any }) {
  const look = () => theme();
  const failure = () => {
    tick();
    return list.failures.get(fail_key(props.target)) ?? null;
  };
  return (
    <If when={failure() != null}>
      <View
        role="button"
        accessibilityLabel={
          failure()
            ? t("error.failed", { what: failure()!.what, error: failure()!.error })
            : ""
        }
        tooltip={
          failure() ? (
            <Tip text={t("error.failed", { what: failure()!.what, error: failure()!.error })} />
          ) : undefined
        }
        class="items-center justify-center rounded-(--radius) hover:bg-accent"
        style={{ width: 20, height: 20, flexShrink: 0, cursor: "default" }}
        onPress={(e: any) => {
          e?.stopPropagation?.();
          list.set_failure(props.target, null);
        }}
      >
        <SmeltIcon path={icon("circle-alert")} size={13} color={look().palette.bad} />
      </View>
    </If>
  );
}

function CollapsedSummary(props: { status: string; count: number; selected?: boolean }) {
  const look = () => theme();
  return (
    <View class="flex-row items-center" style={{ gap: 6, flexShrink: 0 }}>
      <If when={props.status !== "idle"}>
        <View
          style={{
            width: 6,
            height: 6,
            borderRadius: 3,
            backgroundColor: dotColor(look().palette, props.status),
          }}
        />
      </If>
      <If when={props.count > 0}>
        <Text
          style={{
            color: rowFaint(look().palette, !!props.selected),
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
          }}
        >
          {String(props.count)}
        </Text>
      </If>
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
  const anchorEl = signal<any>(null);
  let inputEl: any = null;
  let initialSelected = false;
  let edited = false;
  return (
    <View
      ref={(el: any) => anchorEl.set(el ?? null)}
      class="flex-1 min-w-0"
      onPress={(e: any) => e?.stopPropagation?.()}
      onPressDown={(e: any) => e?.stopPropagation?.()}
    >
      <Input
        ref={(el: any) => {
          inputEl = el ?? null;
          // 同步请求焦点，保证右键菜单关闭时的 focus_back 不会把焦点抢回行容器。
          el?.focus?.();
        }}
        testID="inline-rename-input"
        accessibilityLabel={t("menu.rename")}
        autoFocus={true}
        value={props.rename()?.value ?? ""}
        onLayout={() => {
          // 滚动容器开启 content_culling 时，首轮 commit 的 Command 早于 InputValue 下发；
          // 在首帧 onLayout 回调里执行 setSelection，会在同一 engine 帧末尾紧跟 InputValue 合并下发。
          if (initialSelected || edited || !inputEl) return;
          initialSelected = true;
          const len = props.rename()?.value.length ?? 0;
          if (len > 0) inputEl.setSelection?.(0, len);
        }}
        onChange={({ value }: { value: string }) => {
          edited = true;
          props.onChange(value);
        }}
        onSubmit={() => props.onCommit()}
        onBlur={() => props.onCommit()}
        onKeyPress={(e: any) => {
          if (e?.key === "Escape") props.onCancel();
        }}
        class="rounded-(--radius) border border-input bg-card px-1.5 text-foreground"
        style={{
          height: 22,
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
        }}
      />
      <If when={anchorEl() != null}>
        <Overlay
          anchor={anchorEl()}
          onPressOutside={() => props.onCommit()}
          onCancel={() => props.onCancel()}
          style={{ width: 0, height: 0, pointerEvents: "none" }}
        />
      </If>
    </View>
  );
}

function ProjectSearchInput(props: {
  value: () => string;
  onChange: (value: string) => void;
  onSubmit: () => void;
  onClose: () => void;
  onClearAndClose: () => void;
}) {
  const look = () => theme();
  const anchorEl = signal<any>(null);
  const focused = signal(true);
  let inputEl: any = null;
  return (
    <View
      ref={(el: any) => anchorEl.set(el ?? null)}
      class="flex-1 min-w-0"
      style={{ marginLeft: -6 }}
      onPressDown={(e: any) => e?.stopPropagation?.()}
    >
      <Input
        ref={(el: any) => {
          inputEl = el ?? null;
          el?.focus?.();
        }}
        testID="project-search-input"
        autoFocus={true}
        placeholder={t("list.search_projects")}
        value={props.value()}
        onChange={({ value }: { value: string }) => props.onChange(value)}
        onSubmit={() => props.onSubmit()}
        onFocus={() => focused.set(true)}
        onBlur={() => {
          focused.set(false);
          if (props.value().trim() === "") props.onClose();
        }}
        onKeyPress={(e: any) => {
          if (e?.key === "Escape") props.onClearAndClose();
        }}
        class="rounded-(--radius) border border-input bg-card px-1.5 text-foreground"
        style={{
          height: 22,
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
        }}
      />
      <If when={anchorEl() != null && (focused() || props.value().trim() === "")}>
        <Overlay
          anchor={anchorEl()}
          onPressOutside={() => {
            if (props.value().trim() === "") {
              props.onClose();
            } else {
              focused.set(false);
              inputEl?.blur?.();
            }
          }}
          onCancel={() => props.onClearAndClose()}
          style={{ width: 0, height: 0, pointerEvents: "none" }}
        />
      </If>
    </View>
  );
}

/** 跟随光标的悬浮拖拽卡片（挂在 <Overlay> 上，pointerEvents: "none"）。 */
function DragPreviewCard(props: { drag: () => DragState | null }) {
  const look = () => theme();
  const d = props.drag;
  return (
    <If when={d() != null && d()!.active}>
      <View
        class="shadow-floating"
        style={{
          position: "absolute",
          left: d()!.previewLeft,
          top: d()!.pointerY - d()!.grabOffsetY,
          width: d()!.previewWidth,
          height: d()!.previewHeight,
          paddingLeft: EDGE,
          paddingRight: EDGE,
          borderRadius: look().radius.row,
          backgroundColor: look().palette.surface,
          borderWidth: 1,
          borderColor: look().palette.hair,
          opacity: 0.65,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: space(2),
          pointerEvents: "none",
        }}
      >
        <View
          style={{
            width: LEAD,
            height: LEAD,
            alignItems: "center",
            justifyContent: "center",
            flexShrink: 0,
          }}
        >
          <SmeltIcon
            path={d()!.previewIcon}
            size={14}
            color={d()!.previewIconColor}
            opacity={0.75}
          />
        </View>
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
          {d()!.previewTitle}
        </Text>
        <If when={Boolean(d()!.previewSub)}>
          <Text
            style={{
              color: look().palette.faint,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              flexShrink: 0,
              numberOfLines: 1,
            }}
          >
            {d()!.previewSub ?? ""}
          </Text>
        </If>
      </View>
    </If>
  );
}

interface SharedRowProps {
  sidebar: () => any;
  showAvatar: () => boolean;
  visible: () => boolean;
  rename: () => RenameState | null;
  startRename: (target: RenameState["target"], original: string) => void;
  updateRename: (value: string) => void;
  commitRename: () => void;
  cancelRename: () => void;
  ask: (request: ConfirmRequest) => void;
  drag: () => DragState | null;
  dragSessionPending: () => boolean;
  dragProjectPending: () => boolean;
  beginDrag: (info: Omit<DragState, "active" | "overKey" | "overPayload" | "before" | "lastTargetY">) => void;
  hoverDrag: (dragKind: string | null, key: string, payload: any, localY: number, rowHeight: number, pageY: number) => void;
  consumeSuppressedPress: () => boolean;
  motion: MotionAccess;
  registerRowRef: (key: string, el: any) => void;
}

function SessionRow(props: SharedRowProps & { row: () => any; level: number }) {
  const row = props.row;
  const look = () => theme();
  const id = () => session_flip(row().key);
  const selected = () => {
    tick();
    const active = selected_session(list.selection);
    return active != null && same_conversation(active, row().key);
  };
  const renaming = () => isRenamingTarget(props.rename(), { kind: "conversation", key: row().key });
  const contributed = signal<ContributedMenuItem[]>([]);
  const glowing = signal(false);
  let lastGlowSerial = 0;
  let glowTimer: any = null;
  effect(() => {
    const serial = props.motion.glowSerial(id()) ?? 0;
    if (serial > 0 && serial !== lastGlowSerial) {
      lastGlowSerial = serial;
      glowing.set(true);
      if (glowTimer != null) clearTimeout(glowTimer);
      glowTimer = setTimeout(() => {
        glowTimer = null;
        glowing.set(false);
      }, 450);
    }
  });
  cleanup(() => {
    if (glowTimer != null) clearTimeout(glowTimer);
  });
  let rowWidth = 220;
  let rowHeight = ROW_MD;

  const myDragKind = () => sessionDragKind(props.sidebar(), row());
  const isDragged = () => {
    const d = props.drag();
    return d != null && d.active && d.kind === "session" && d.key === id();
  };
  const dropEdge = (): "top" | "bottom" | null => {
    const d = props.drag();
    const dk = myDragKind();
    if (!d || !d.active || !dk || d.dragKind !== dk || d.overKey !== id() || d.key === id()) {
      return null;
    }
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

  const unread = () => row_tally(row()).unread > 0;
  const loud = () =>
    row().status === "needs_you" || row().status === "running" || unread();
  const quiet = () => !selected() && !loud();
  const hasAvatar = computed(() => props.showAvatar());
  const initialKey = row().key;
  const actor = computed(() => avatarActor(initialKey.machine, initialKey.id));
  const info = () => rowMeta(row(), selected());
  const indentLevel = () => (hasAvatar() ? Math.max(props.level - 1, 0) : props.level);
  const iconAlpha = () => (isDragged() ? 0.4 : 1);
  const leadIconColor = () =>
    row().status === "idle"
      ? rowMuted(look().palette, selected())
      : dotColor(look().palette, row().status);

  cleanup(() => props.registerRowRef(id(), null));

  return (
    <SmeltContextMenu
      popupStyle={{ minWidth: 184 }}
      onOpenChange={({ open }) => {
        if (open) loadContributed();
      }}
      trigger={
        <View
          ref={(el: any) => props.registerRowRef(id(), el)}
          role="button"
          testID={`session-row:${row().key.id}`}
          tooltip={props.rename() == null && row().tooltip ? <Tip text={row().tooltip} /> : undefined}
          onLayout={({ bounds }: any) => {
            if (bounds?.size) {
              rowWidth = bounds.size.width;
              rowHeight = bounds.size.height;
            }
          }}
          onPress={(e: any) => {
            if (props.consumeSuppressedPress()) return;
            if (e?.clickCount === 2) {
              props.startRename({ kind: "conversation", key: row().key }, row().title);
              return;
            }
            triggerOpen();
          }}
          onPressDown={(e: any) => {
            if (renaming()) return;
            const dk = myDragKind();
            if (!dk) return;
            const pageX = e?.pagePosition?.x ?? 0;
            const pageY = e?.pagePosition?.y ?? 0;
            const localX = e?.position?.x ?? 0;
            const localY = e?.position?.y ?? rowHeight / 2;
            const cur = row();
            props.beginDrag({
              kind: "session",
              dragKind: dk,
              key: id(),
              payload: cur,
              previewTitle: cur.title,
              previewSub: info().text,
              previewIcon: iconPath(cur.icon),
              previewIconColor: leadIconColor(),
              previewHeight: rowHeight || (hasAvatar() ? AVATAR_ROW : ROW_MD),
              previewWidth: rowWidth,
              previewLeft: pageX - localX,
              grabOffsetY: localY,
              startY: pageY,
              pointerY: pageY,
            });
          }}
          onMouseMove={
            props.dragSessionPending()
              ? (e: any) => {
                  const d = props.drag();
                  if (!d || d.kind !== "session") return;
                  const localY = e?.position?.y ?? 0;
                  const pageY = e?.pagePosition?.y ?? 0;
                  props.hoverDrag(
                    myDragKind(),
                    id(),
                    row(),
                    localY,
                    rowHeight || (hasAvatar() ? AVATAR_ROW : ROW_MD),
                    pageY,
                  );
                }
              : undefined
          }
          class={`group/row relative flex-row items-center ${
            selected() ? "bg-sidebar-accent" : "hover:bg-accent active:bg-sidebar-accent"
          }`}
          style={{
            height: hasAvatar() ? AVATAR_ROW : ROW_MD,
            paddingLeft: indent(indentLevel()),
            paddingRight: space(1),
            gap: space(2),
            borderRadius: look().radius.row,
            opacity: isDragged() ? 0.4 : 1,
            cursor: "default",
          }}
        >
          <WashLayer serial={() => props.motion.washSerial(id())} />
          <DropLine edge={dropEdge()} />

          <If
            when={hasAvatar()}
            else={
              <View
                tooltip={
                  props.rename() == null && providerTip(row().icon) ? (
                    <Tip text={providerTip(row().icon)!} />
                  ) : undefined
                }
                style={{
                  width: LEAD,
                  height: LEAD,
                  alignItems: "center",
                  justifyContent: "center",
                  flexShrink: 0,
                }}
              >
                <SmeltIcon
                  path={iconPath(row().icon)}
                  size={LEAD}
                  color={glowing() ? look().palette.ink : leadIconColor()}
                  opacity={iconAlpha()}
                  class="transition-colors duration-300"
                />
              </View>
            }
          >
            <View
              tooltip={
                props.rename() == null && providerTip(row().icon) ? (
                  <Tip text={providerTip(row().icon)!} />
                ) : undefined
              }
              style={{
                width: AVATAR,
                height: AVATAR,
                flexShrink: 0,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <Avatar
                seed={actor().seed}
                act={actor().act}
                since={actor().since}
                active={props.visible()}
                opacity={iconAlpha()}
                size={AVATAR}
                eye={row().icon?.kind === "provider" ? row().icon.path : undefined}
              />
            </View>
          </If>

          <If
            when={renaming()}
            else={
              <View class="flex-1 min-w-0 flex-row items-center" style={{ gap: space(1.5) }}>
                <Text
                  class="flex-1 min-w-0"
                  style={{
                    color:
                      row().unavailable != null
                        ? rowFaint(look().palette, selected())
                        : quiet()
                          ? rowMuted(look().palette, selected())
                          : look().palette.text,
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    fontWeight:
                      selected() || loud()
                        ? look().type.ui_medium.weight
                        : look().type.ui.weight,
                    numberOfLines: 1,
                  }}
                >
                  {row().title}
                </Text>
                <If when={Boolean(row().pinned)}>
                  <SmeltIcon
                    path={icon("pin")}
                    size={11}
                    color={look().palette.faint}
                    opacity={iconAlpha()}
                  />
                </If>
                <If when={row().machine_name != null}>
                  <Text
                    style={{
                      color: rowMuted(look().palette, selected()),
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      flexShrink: 0,
                      numberOfLines: 1,
                    }}
                  >
                    {row().machine_name ?? ""}
                  </Text>
                </If>
                <If when={row().origin != null}>
                  <View class="flex-row items-center" style={{ gap: 4, flexShrink: 0 }}>
                    <If when={Boolean(row().origin?.workset)}>
                      <View
                        tooltip={
                          row().origin?.workset?.tooltip ? (
                            <Tip text={row().origin.workset.tooltip} />
                          ) : undefined
                        }
                        class="flex-row items-center"
                        style={{
                          gap: 3,
                          paddingHorizontal: 5,
                          height: 18,
                          borderRadius: look().radius.sm,
                          backgroundColor: look().palette.hair,
                          flexShrink: 0,
                        }}
                      >
                        <SmeltIcon
                          path={icon("git-branch")}
                          size={10}
                          color={rowMuted(look().palette, selected())}
                          opacity={iconAlpha()}
                        />
                        <Text
                          style={{
                            color: rowMuted(look().palette, selected()),
                            fontSize: look().type.text_xsmall.size,
                            lineHeight: look().type.text_xsmall.line_height,
                            numberOfLines: 1,
                          }}
                        >
                          {row().origin?.workset?.task ?? ""}
                        </Text>
                      </View>
                    </If>
                    <Text
                      style={{
                        color: rowFaint(look().palette, selected()),
                        fontSize: look().type.text_xsmall.size,
                        lineHeight: look().type.text_xsmall.line_height,
                        numberOfLines: 1,
                      }}
                    >
                      {row().origin?.project ?? ""}
                    </Text>
                  </View>
                </If>
              </View>
            }
          >
            <InlineRenameInput
              rename={props.rename}
              onChange={props.updateRename}
              onCommit={props.commitRename}
              onCancel={props.cancelRename}
            />
          </If>

          <FailureMark target={{ kind: "conversation", key: row().key }} />

          {/* 行尾信息与悬停关闭按钮原位叠放，切透明度不挪宽。 */}
          <View
            style={{
              position: "relative",
              minWidth: 22,
              height: 22,
              flexShrink: 0,
              justifyContent: "center",
              alignItems: "flex-end",
            }}
          >
            <View
              class={
                renaming()
                  ? "flex-row items-center opacity-100"
                  : "flex-row items-center opacity-100 group-hover/row:opacity-0"
              }
              style={{ gap: 4, pointerEvents: "none" }}
            >
              <If when={Boolean(info().unread)}>
                <View
                  style={{
                    width: 6,
                    height: 6,
                    borderRadius: 3,
                    backgroundColor: look().palette.accent,
                  }}
                />
              </If>
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

            <If when={!renaming()}>
              <View class="absolute inset-0 items-end justify-center opacity-0 invisible pointer-events-none group-hover/row:opacity-100 group-hover/row:visible group-hover/row:pointer-events-auto">
                <SmeltButton
                  dismiss={true}
                  icon={icon("close")}
                  tooltip={t("close.confirm")}
                  accessibilityLabel={t("close.confirm")}
                  testID={`close-session:${row().key.id}`}
                  onPress={(e: any) => {
                    e?.stopPropagation?.();
                    triggerClose();
                  }}
                />
              </View>
            </If>
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
      <MenuDivider />
      <MenuEntry
        label={t("close.confirm")}
        iconPath={icon("close")}
        danger={true}
        onSelect={triggerClose}
      />
    </SmeltContextMenu>
  );
}

function ProjectSection(props: SharedRowProps & { project: () => any; byMachine: boolean }) {
  const project = props.project;
  const look = () => theme();
  const key = () => project_flip(project().machine, project().root);
  const selected = () => {
    tick();
    const sel = list.selection;
    return sel.kind === "project" && sel.machine === project().machine && sel.root === project().root;
  };
  const collapsible = () => project().items.length > 0;
  const collapsed = computed(() => project().collapsed);
  const popoverOpen = signal(false);
  const contributed = signal<ContributedMenuItem[]>([]);
  const machineItems = signal<ContributedMenuItem[]>([]);
  let rowWidth = 220;
  let rowHeight = ROW_MD;

  const myDragKind = () => projectDragKind(props.sidebar(), project());
  const isDragged = () => {
    const d = props.drag();
    return d != null && d.active && d.kind === "project" && d.key === key();
  };
  const dropEdge = (): "top" | "bottom" | null => {
    const d = props.drag();
    if (!d || !d.active || d.dragKind !== myDragKind() || d.overKey !== key() || d.key === key()) {
      return null;
    }
    return d.before ? "top" : "bottom";
  };

  const loadContributed = () => {
    const cur = project();
    if (cur.root) {
      fetchContextMenuItems(PROJECT_MENU, cur.machine, { machine: cur.machine, path: cur.root }).then(
        (items) => contributed.set(items),
      );
    } else {
      contributed.set([]);
    }
    if (cur.machine_name != null) {
      fetchContextMenuItems(MACHINE_MENU, cur.machine, { machine: cur.machine }).then((items) =>
        machineItems.set(items),
      );
    } else {
      machineItems.set([]);
    }
  };

  const toggleCollapse = () => {
    const cur = project();
    if (!collapsible()) return;
    list.set_collapsed({ kind: "project", machine: cur.machine, root: cur.root }, !cur.collapsed);
  };

  const selectProject = () => {
    const cur = project();
    if (cur.root === "") {
      toggleCollapse();
      return;
    }
    list.report_row_failure(
      { kind: "project", machine: cur.machine, root: cur.root },
      t("action.select_project"),
      list.select_project(cur.machine, cur.root),
    );
  };

  const areaLevel = () => (props.byMachine ? 1 : 0);
  const projectVisible = computed(() => props.visible() && !collapsed());
  const baseFolderIcon = () =>
    icon(collapsible() && !project().collapsed ? "folder-open" : "folder");
  const iconAlpha = () => (isDragged() ? 0.4 : 1);

  cleanup(() => props.registerRowRef(key(), null));

  return (
    <SmeltDisclosure
      open={!collapsed()}
     
      content={
        <View style={{ gap: 2, paddingTop: 2 }}>
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
                  level={areaLevel() + 1}
                  sidebar={props.sidebar}
                  showAvatar={props.showAvatar}
                  visible={projectVisible}
                  rename={props.rename}
                  startRename={props.startRename}
                  updateRename={props.updateRename}
                  commitRename={props.commitRename}
                  cancelRename={props.cancelRename}
                  ask={props.ask}
                  drag={props.drag}
                  dragSessionPending={props.dragSessionPending}
                  dragProjectPending={props.dragProjectPending}
                  beginDrag={props.beginDrag}
                  hoverDrag={props.hoverDrag}
                  consumeSuppressedPress={props.consumeSuppressedPress}
                  motion={props.motion}
                  registerRowRef={props.registerRowRef}
                />
              ) : (
                <WorksetSection
                  workset={() => item().workset}
                  level={areaLevel() + 1}
                  sidebar={props.sidebar}
                  showAvatar={props.showAvatar}
                  visible={projectVisible}
                  rename={props.rename}
                  startRename={props.startRename}
                  updateRename={props.updateRename}
                  commitRename={props.commitRename}
                  cancelRename={props.cancelRename}
                  ask={props.ask}
                  drag={props.drag}
                  dragSessionPending={props.dragSessionPending}
                  dragProjectPending={props.dragProjectPending}
                  beginDrag={props.beginDrag}
                  hoverDrag={props.hoverDrag}
                  consumeSuppressedPress={props.consumeSuppressedPress}
                  motion={props.motion}
                  registerRowRef={props.registerRowRef}
                />
              )
            }
          </Each>
        </View>
      }
    >
      <SmeltContextMenu
        popupStyle={{ minWidth: 184 }}
        onOpenChange={({ open }) => {
          if (open) loadContributed();
        }}
        trigger={
          <View
            ref={(el: any) => props.registerRowRef(key(), el)}
            role="button"
            testID={`project-row:${project().root}`}
            onLayout={({ bounds }: any) => {
              if (bounds?.size) {
                rowWidth = bounds.size.width;
                rowHeight = bounds.size.height;
              }
            }}
            onPress={() => {
              if (props.consumeSuppressedPress()) return;
              selectProject();
            }}
            onPressDown={(e: any) => {
              const pageX = e?.pagePosition?.x ?? 0;
              const pageY = e?.pagePosition?.y ?? 0;
              const localX = e?.position?.x ?? 0;
              const localY = e?.position?.y ?? rowHeight / 2;
              const cur = project();
              props.beginDrag({
                kind: "project",
                dragKind: myDragKind(),
                key: key(),
                payload: cur,
                previewTitle: cur.name,
                previewSub: cur.branch ?? null,
                previewIcon: baseFolderIcon(),
                previewIconColor: look().palette.muted,
                previewHeight: rowHeight || ROW_MD,
                previewWidth: rowWidth,
                previewLeft: pageX - localX,
                grabOffsetY: localY,
                startY: pageY,
                pointerY: pageY,
              });
            }}
            onMouseMove={
              props.dragProjectPending()
                ? (e: any) => {
                    const d = props.drag();
                    if (!d || d.kind !== "project") return;
                    const localY = e?.position?.y ?? 0;
                    const pageY = e?.pagePosition?.y ?? 0;
                    props.hoverDrag(myDragKind(), key(), project(), localY, rowHeight || ROW_MD, pageY);
                  }
                : undefined
            }
            class={`group/row relative flex-row items-center ${
              selected() ? "bg-sidebar-accent" : "hover:bg-accent"
            }`}
            style={{
              height: ROW_MD,
              paddingLeft: indent(areaLevel()),
              paddingRight: space(1),
              gap: space(2),
              borderRadius: look().radius.row,
              opacity: isDragged() ? 0.4 : 1,
              cursor: "default",
            }}
          >
            <DropLine edge={dropEdge()} />

            <DisclosureSlot
              baseIcon={baseFolderIcon()}
              foldKey={key()}
              collapsed={project().collapsed}
              collapsible={collapsible()}
              opacity={iconAlpha()}
              onToggle={toggleCollapse}
            />

            <View class="flex-1 min-w-0 flex-row items-center" style={{ gap: space(1.5) }}>
              <Text
                class="min-w-0"
                style={{
                  color:
                    project().link?.unavailable != null
                      ? rowFaint(look().palette, selected())
                      : !collapsible()
                        ? rowMuted(look().palette, selected())
                        : look().palette.text,
                  fontSize: look().type.ui.size,
                  lineHeight: look().type.ui.line_height,
                  fontWeight: selected()
                    ? look().type.ui_medium.weight
                    : look().type.ui.weight,
                  flexShrink: 1,
                  numberOfLines: 1,
                }}
              >
                {project().name}
              </Text>
              <If when={Boolean(project().pinned)}>
                <SmeltIcon
                  path={icon("pin")}
                  size={11}
                  color={look().palette.faint}
                  opacity={iconAlpha()}
                />
              </If>
              <If when={!props.byMachine && project().machine_name != null}>
                <View
                  tooltip={
                    project().link?.detail ? <Tip text={project().link.detail} /> : undefined
                  }
                  onMouseEnter={refreshConnectionsThrottled}
                  class="flex-row items-center"
                  style={{ gap: 4, flexShrink: 0 }}
                >
                  <If when={Boolean(project().os_icon)}>
                    <SmeltIcon
                      path={project().os_icon}
                      size={12}
                      color={lightColor(look().palette, project().link?.light ?? "good")}
                      opacity={iconAlpha()}
                    />
                  </If>
                  <Text
                    style={{
                      color: rowMuted(look().palette, selected()),
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      flexShrink: 0,
                      numberOfLines: 1,
                    }}
                  >
                    {project().machine_name ?? ""}
                  </Text>
                </View>
              </If>
              <If when={Boolean(project().branch)}>
                <View
                  tooltip={<Tip text={project().branch ?? ""} />}
                  style={{ maxWidth: 120, flexShrink: 1, minWidth: 0 }}
                >
                  <Text
                    style={{
                      color: rowFaint(look().palette, selected()),
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      numberOfLines: 1,
                    }}
                  >
                    {project().branch ?? ""}
                  </Text>
                </View>
              </If>
              <If when={(project().repositories ?? 0) > 1}>
                <Text
                  style={{
                    color: rowFaint(look().palette, selected()),
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    flexShrink: 0,
                    numberOfLines: 1,
                  }}
                >
                  {t("project.repositories.other", { count: project().repositories })}
                </Text>
              </If>
            </View>

            <FailureMark target={{ kind: "project", machine: project().machine, root: project().root }} />

            <If when={project().collapsed}>
              <View class={popoverOpen() ? "opacity-0" : "opacity-100 group-hover/row:opacity-0"}>
                <CollapsedSummary
                  status={project().status}
                  count={project().sessions}
                  selected={selected()}
                />
              </View>
            </If>

            <If when={Boolean(project().root)}>
              <NewSessionPopover
                machine={project().machine}
                path={project().root}
                tooltip={t("menu.new_session")}
                hoverClass="opacity-0 group-hover/row:opacity-100"
                onOpenChange={(open) => popoverOpen.set(open)}
              />
            </If>
          </View>
        }
      >
        {project().root ? (
          <>
            <MenuEntry
              label={t("menu.new_workset")}
              iconPath={icon("git-branch")}
              onSelect={() => list.open_repositories(project().machine, project().root, "create")}
            />
            <MenuEntry
              label={t("menu.manage_repositories")}
              iconPath={icon("repositories")}
              onSelect={() => list.open_repositories(project().machine, project().root, "manage")}
            />
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
              label={t("menu.copy_project_path")}
              iconPath={icon("copy")}
              onSelect={() => clipboardWrite(project().root).catch(() => {})}
            />
            {project().branch ? (
              <MenuEntry
                label={t("menu.copy_branch")}
                iconPath={icon("git-branch")}
                onSelect={() => clipboardWrite(project().branch).catch(() => {})}
              />
            ) : null}
          </>
        ) : null}
        {project().machine_name != null && machineItems().length > 0 ? (
          <>
            <MenuDivider />
            <Submenu
              trigger={
                <View class={MENU_ITEM_CLASS} style={{ height: 28, gap: space(2) }}>
                  <Text
                    class="flex-1 min-w-0"
                    style={{
                      color: look().palette.text,
                      fontSize: look().type.ui.size,
                      lineHeight: look().type.ui.line_height,
                      numberOfLines: 1,
                    }}
                  >
                    {t("menu.machine", { name: project().machine_name })}
                  </Text>
                  <SmeltIcon path={icon("chevron-right")} size={12} color={look().palette.muted} />
                </View>
              }
            >
              <ContributedMenuItems
                items={machineItems()}
                leadingDivider={false}
                onFail={(command, err) =>
                  list.fail_row(
                    { kind: "project", machine: project().machine, root: project().root },
                    t("action.run_command", { command }),
                    message(err),
                  )
                }
              />
            </Submenu>
          </>
        ) : null}
        {project().root ? <MenuDivider /> : null}
        <MenuEntry
          label={project().pinned ? t("menu.unpin") : t("menu.pin")}
          iconPath={icon(project().pinned ? "pin-off" : "pin")}
          onSelect={() => actions.pin_project(list, project().machine, project().root, !project().pinned)}
        />
        {project().root ? (
          <>
            <MenuDivider />
            <MenuEntry
              label={project().opened ? t("menu.close_project") : t("menu.close_group")}
              iconPath={icon("close")}
              danger={true}
              onSelect={() => {
                const cur = project();
                actions.close_project(list, props.ask, cur.machine, cur.root, cur.name, cur.sessions);
              }}
            />
          </>
        ) : null}
      </SmeltContextMenu>
    </SmeltDisclosure>
  );
}

function WorksetSection(props: SharedRowProps & { workset: () => any; level: number }) {
  const workset = props.workset;
  const look = () => theme();
  const key = () => workset_flip(workset().machine, workset().id);
  const collapsible = () => workset().sessions.length > 0;
  const collapsed = computed(() => workset().collapsed);
  const worksetVisible = computed(() => props.visible() && !collapsed());
  const popoverOpen = signal(false);
  const renaming = () =>
    isRenamingTarget(props.rename(), {
      kind: "workset",
      machine: workset().machine,
      id: workset().id,
    });
  const repos = () => (Array.isArray(workset().repositories) ? workset().repositories : []);

  cleanup(() => props.registerRowRef(key(), null));

  return (
    <SmeltDisclosure
      open={!collapsed()}
     
      content={
        <View style={{ gap: 2, paddingTop: 2 }}>
          <Each of={workset().sessions} key={(row: any) => session_flip(row.key)}>
            {(row: () => any) => (
              <SessionRow
                row={row}
                level={props.level + 1}
                sidebar={props.sidebar}
                showAvatar={props.showAvatar}
                visible={worksetVisible}
                rename={props.rename}
                startRename={props.startRename}
                updateRename={props.updateRename}
                commitRename={props.commitRename}
                cancelRename={props.cancelRename}
                ask={props.ask}
                drag={props.drag}
                dragSessionPending={props.dragSessionPending}
                dragProjectPending={props.dragProjectPending}
                beginDrag={props.beginDrag}
                hoverDrag={props.hoverDrag}
                consumeSuppressedPress={props.consumeSuppressedPress}
                motion={props.motion}
                registerRowRef={props.registerRowRef}
              />
            )}
          </Each>
        </View>
      }
    >
      <SmeltContextMenu
        popupStyle={{ minWidth: 184 }}
        trigger={
          <View
            ref={(el: any) => props.registerRowRef(key(), el)}
            role="button"
            tooltip={props.rename() == null && workset().tooltip ? <Tip text={workset().tooltip} /> : undefined}
            onPress={() => {
              if (renaming() || !collapsible()) return;
              list.set_collapsed(
                { kind: "workset", machine: workset().machine, id: workset().id },
                !workset().collapsed,
              );
            }}
            class="group/row flex-row items-center hover:bg-accent"
            style={{
              height: ROW_MD,
              paddingLeft: indent(props.level),
              paddingRight: space(1),
              gap: space(2),
              borderRadius: look().radius.row,
              cursor: "default",
            }}
          >
            <DisclosureSlot
              baseIcon={icon("git-branch")}
              baseColor={workset().merge === "merged" ? look().palette.ok : look().palette.muted}
              baseSize={13}
              foldKey={key()}
              collapsed={workset().collapsed}
              collapsible={collapsible()}
            />

            <If
              when={renaming()}
              else={
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
                  <If when={Boolean(workset().pinned)}>
                    <SmeltIcon path={icon("pin")} size={11} color={look().palette.faint} />
                  </If>
                </View>
              }
            >
              <InlineRenameInput
                rename={props.rename}
                onChange={props.updateRename}
                onCommit={props.commitRename}
                onCancel={props.cancelRename}
              />
            </If>

            <If when={workset().collapsed}>
              <View class={popoverOpen() ? "opacity-0" : "opacity-100 group-hover/row:opacity-0"}>
                <CollapsedSummary status={workset().status} count={workset().sessions.length} />
              </View>
            </If>

            <If when={workset().merge === "merged"}>
              <View
                role="button"
                accessibilityLabel={t("list.finish_merged")}
                tooltip={<Tip text={t("list.finish_merged")} />}
                class="items-center justify-center rounded-(--radius) hover:bg-accent"
                style={{ width: 24, height: 24, flexShrink: 0, cursor: "default" }}
                onPress={(e: any) => {
                  e?.stopPropagation?.();
                  actions.finish_workset(
                    list,
                    props.ask,
                    workset().machine,
                    workset().id,
                    workset().task,
                  );
                }}
              >
                <SmeltIcon path={icon("circle-check")} size={14} color={look().palette.ok} />
              </View>
            </If>

            <If when={Boolean(workset().path)}>
              <NewSessionPopover
                machine={workset().machine}
                path={workset().path}
                tooltip={t("list.new_in_workset")}
                hoverClass="opacity-0 group-hover/row:opacity-100"
                onOpenChange={(open) => popoverOpen.set(open)}
              />
            </If>

            <FailureMark target={{ kind: "workset", machine: workset().machine, id: workset().id }} />
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
            props.startRename(
              { kind: "workset", machine: workset().machine, id: workset().id },
              workset().task,
            )
          }
        />
        <MenuDivider />
        {repos().length <= 1 ? (
          <MenuEntry
            label={t("menu.create_pr")}
            iconPath={icon("external-link")}
            disabled={repos().length === 0}
            onSelect={() => {
              const first = repos()[0];
              if (first) {
                actions.open_pull_request(list, workset().machine, workset().id, first.worktree);
              }
            }}
          />
        ) : (
          <Submenu
            trigger={
              <View class={MENU_ITEM_CLASS} style={{ height: 28, gap: space(2) }}>
                <View style={{ width: 14, height: 14, flexShrink: 0 }}>
                  <SmeltIcon path={icon("external-link")} size={14} color={look().palette.muted} />
                </View>
                <Text
                  class="flex-1 min-w-0"
                  style={{
                    color: look().palette.text,
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    numberOfLines: 1,
                  }}
                >
                  {t("menu.create_pr")}
                </Text>
                <SmeltIcon path={icon("chevron-right")} size={12} color={look().palette.muted} />
              </View>
            }
          >
            <Each of={repos()} key={(r: any) => r.worktree}>
              {(repo: () => any) => (
                <MenuEntry
                  label={repo().name}
                  onSelect={() =>
                    actions.open_pull_request(
                      list,
                      workset().machine,
                      workset().id,
                      repo().worktree,
                    )
                  }
                />
              )}
            </Each>
          </Submenu>
        )}
        <MenuEntry
          label={workset().checking ? t("menu.checking_merge") : t("menu.check_merge")}
          iconPath={icon("refresh")}
          disabled={workset().checking}
          onSelect={() => actions.check_workset(list, workset().machine, workset().id)}
        />
        <MenuEntry
          label={t("menu.finish_workset")}
          iconPath={icon("close")}
          danger={true}
          onSelect={() =>
            actions.finish_workset(list, props.ask, workset().machine, workset().id, workset().task)
          }
        />
      </SmeltContextMenu>
    </SmeltDisclosure>
  );
}

/** 按机器分开时的机器标题行：系统图标（悬停换箭头）、名称、状态、悬停添加项目/机器菜单 + 右键菜单。 */
function MachineHeaderRow(props: {
  machine: () => any;
  area: () => any;
  registerRowRef: (key: string, el: any) => void;
}) {
  const look = () => theme();
  const key = () => machine_flip(props.machine().machine);
  const menuItems = signal<ContributedMenuItem[]>([]);
  const menuOpen = signal(false);

  const loadMenu = () => {
    const m = props.machine().machine;
    fetchContextMenuItems(MACHINE_MENU, m, { machine: m }).then((items) => menuItems.set(items));
  };

  effect(() => {
    machineMenuVersion();
    loadMenu();
  });

  cleanup(() => props.registerRowRef(key(), null));

  const statusText = () =>
    props.machine().link?.unavailable ??
    (props.machine().received ? null : t("machine.connecting"));
  const collapsible = () => Boolean(props.machine().received);

  return (
    <SmeltContextMenu
      popupStyle={{ minWidth: 184 }}
      onOpenChange={({ open }) => {
        if (open) loadMenu();
      }}
      trigger={
        <View
          ref={(el: any) => props.registerRowRef(key(), el)}
          role="button"
          class="group/row flex-row items-center hover:bg-accent"
          style={{
            height: ROW_SM,
            paddingHorizontal: space(2),
            marginTop: space(1),
            gap: space(2),
            borderRadius: look().radius.row,
            cursor: "default",
          }}
          onPress={() => {
            if (!collapsible()) return;
            list.set_machine_collapsed(props.machine().machine, !props.area().collapsed);
          }}
        >
          <DisclosureSlot
            baseIcon={props.machine().os_icon}
            baseColor={lightColor(look().palette, props.machine().link?.light ?? "good")}
            baseTooltip={props.machine().link?.detail ?? null}
            onBaseHover={refreshConnectionsThrottled}
            foldKey={key()}
            collapsed={props.area().collapsed}
            collapsible={collapsible()}
          />
          <Text
            class="flex-1 min-w-0"
            style={{
              color: look().palette.text,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              fontWeight: look().type.text_xsmall_medium.weight,
              numberOfLines: 1,
            }}
          >
            {props.machine().name}
          </Text>
          <If when={statusText() != null}>
            <Text
              style={{
                color: look().palette.faint,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                flexShrink: 0,
              }}
            >
              {statusText() ?? ""}
            </Text>
          </If>
          <If when={props.area().collapsed}>
            <View class={menuOpen() ? "opacity-0" : "opacity-100 group-hover/row:opacity-0"}>
              <CollapsedSummary status={props.area().status} count={props.area().sessions} />
            </View>
          </If>
          <View
            class={
              menuOpen()
                ? "flex-row items-center opacity-100"
                : "flex-row items-center opacity-0 group-hover/row:opacity-100"
            }
            style={{ flexShrink: 0 }}
          >
            <View
              role="button"
              disabled={props.machine().link?.unavailable != null}
              accessibilityLabel={t("list.add_project")}
              tooltip={<Tip text={t("list.add_project")} />}
              class="items-center justify-center rounded-(--radius) hover:bg-accent"
              style={{
                width: 24,
                height: 24,
                opacity: props.machine().link?.unavailable != null ? 0.4 : 1,
                cursor: "default",
              }}
              onPress={(e: any) => {
                e?.stopPropagation?.();
                if (props.machine().link?.unavailable != null) return;
                actions.open_project(list, props.machine().machine);
              }}
            >
              <SmeltIcon path={icon("folder-plus")} size={14} color={look().palette.muted} />
            </View>
            <If when={menuItems().length > 0}>
              <SmeltMenu
                placement="bottom"
                align="end"
                popupStyle={{ minWidth: 176 }}
                onOpenChange={({ open }) => {
                  menuOpen.set(open);
                  if (open) loadMenu();
                }}
                trigger={
                  <View
                    role="button"
                    accessibilityLabel={t("list.machine")}
                    tooltip={<Tip text={t("list.machine")} />}
                    class={`items-center justify-center rounded-(--radius) ${
                      menuOpen() ? "bg-accent" : "hover:bg-accent"
                    }`}
                    style={{ width: 24, height: 24, cursor: "default" }}
                    onPress={(e: any) => e?.stopPropagation?.()}
                  >
                    <SmeltIcon path={icon("ellipsis")} size={14} color={look().palette.muted} />
                  </View>
                }
              >
                <ContributedMenuItems
                  items={menuItems()}
                  leadingDivider={false}
                  onFail={(command, err) =>
                    list.notify_error(list.failed(t("action.run_command", { command }), message(err)))
                  }
                />
              </SmeltMenu>
            </If>
          </View>
        </View>
      }
    >
      <ContributedMenuItems
        items={menuItems()}
        leadingDivider={false}
        onFail={(command, err) =>
          list.notify_error(list.failed(t("action.run_command", { command }), message(err)))
        }
      />
    </SmeltContextMenu>
  );
}

/** 还没收到工作区目录的机器行（不按机器分开时垫在列表底部，支持右键重连）。 */
function WaitingMachineRow(props: { machine: () => any }) {
  const look = () => theme();
  const menuItems = signal<ContributedMenuItem[]>([]);

  const loadMenu = () => {
    const m = props.machine().machine;
    fetchContextMenuItems(MACHINE_MENU, m, { machine: m }).then((items) => menuItems.set(items));
  };

  return (
    <SmeltContextMenu
      popupStyle={{ minWidth: 184 }}
      onOpenChange={({ open }) => {
        if (open) loadMenu();
      }}
      trigger={
        <View
          tooltip={props.machine().link?.detail ? <Tip text={props.machine().link.detail} /> : undefined}
          onMouseEnter={refreshConnectionsThrottled}
          class="flex-row items-center hover:bg-accent"
          style={{
            height: ROW_SM,
            paddingHorizontal: space(2),
            gap: space(2),
            borderRadius: look().radius.row,
            cursor: "default",
          }}
        >
          <View
            style={{
              width: LEAD,
              height: LEAD,
              alignItems: "center",
              justifyContent: "center",
              flexShrink: 0,
            }}
          >
            <SmeltIcon
              path={props.machine().os_icon}
              size={14}
              color={lightColor(look().palette, props.machine().link?.light ?? "bad")}
            />
          </View>
          <Text
            class="flex-1 min-w-0"
            style={{
              color: look().palette.muted,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              numberOfLines: 1,
            }}
          >
            {props.machine().name}
          </Text>
          <Text
            style={{
              color: look().palette.faint,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              flexShrink: 0,
            }}
          >
            {props.machine().link?.unavailable ?? t("machine.connecting")}
          </Text>
        </View>
      }
    >
      <ContributedMenuItems
        items={menuItems()}
        leadingDivider={false}
        onFail={(command, err) =>
          list.notify_error(list.failed(t("action.run_command", { command }), message(err)))
        }
      />
    </SmeltContextMenu>
  );
}

/** 滚到可视区外时的悬浮提示胶囊（点击直接打开最近的会话）。 */
function OffscreenPill(props: { side: () => { tally: any; nearest: any }; up: boolean }) {
  const look = () => theme();
  const visible = () => props.side().nearest != null && !tally_is_empty(props.side().tally);
  return (
    <If when={visible()}>
      <View
        style={{
          position: "absolute",
          top: props.up ? 6 : undefined,
          bottom: props.up ? undefined : 6,
          left: 0,
          right: 0,
          alignItems: "center",
          justifyContent: "center",
          pointerEvents: "box-none",
        }}
      >
        <View
          role="button"
          class="flex-row items-center rounded-full bg-popover shadow-button-floating hover:bg-soft"
          style={{
            height: 24,
            paddingHorizontal: 10,
            gap: 5,
            cursor: "default",
          }}
          onPress={() => {
            const session = props.side().nearest;
            if (!session) return;
            list.report_row_failure(
              { kind: "conversation", key: session },
              t("action.open_session"),
              list.open_conversation(session),
            );
          }}
        >
          <SmeltIcon
            path={icon(props.up ? "arrow-up" : "arrow-down")}
            size={12}
            color={look().palette.text}
          />
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              fontWeight: look().type.text_xsmall_medium.weight,
            }}
          >
            {tally_label(props.side().tally, t)}
          </Text>
        </View>
      </View>
    </If>
  );
}

/** 列表主视图（对应 `smelt.session-list/main`）。 */
export function main() {
  const searching = signal(false);
  const rename = signal<RenameState | null>(null);
  const confirm = signal<ConfirmRequest | null>(null);
  const drag = signal<DragState | null>(null);
  const groupingOpen = signal(false);
  const addProjectOpen = signal(false);
  const machinesMenuOpen = signal(false);
  const machinesMenuGroups = signal<MachineMenuGroup[]>([]);
  const offscreenAbove = signal<string[]>([]);
  const offscreenBelow = signal<string[]>([]);
  const motionTick = signal(0);

  let rootRef: any = null;
  let scrollRef: any = null;
  let listContentRef: any = null;
  const rowRefs = new Map<string, any>();

  const listMotion = new ListMotion(list.sidebar);
  let seenSidebar = list.sidebar;
  let seenSelected = selected_session(list.selection);
  let suppressPressUntil = 0;
  let hitValidTargetInMove = false;
  let scrollOffsetY = 0;
  let scrollViewportTop = 0;
  let scrollViewportHeight = 0;
  let scrollContentHeight = 0;
  let autoScrollFrame: number | null = null;
  let autoScrollLast = 0;
  let autoScrollDir = 0;
  let measureSerial = 0;

  const stopAutoScroll = () => {
    autoScrollDir = 0;
    if (autoScrollFrame != null) {
      cancelAnimationFrame(autoScrollFrame);
      autoScrollFrame = null;
    }
  };

  // 拖到滚动区边上时跟着显示帧推进；离开边缘、松手就不再要帧。
  const autoScrollStep = (now: number) => {
    autoScrollFrame = null;
    if (!drag()?.active || autoScrollDir === 0) return;
    const elapsed = Math.min(50, Math.max(0, now - autoScrollLast));
    autoScrollLast = now;
    const maxScroll = Math.max(0, scrollContentHeight - scrollViewportHeight);
    const nextY = Math.max(0, Math.min(maxScroll, scrollOffsetY + autoScrollDir * AUTOSCROLL_SPEED * elapsed));
    if (nextY !== scrollOffsetY) {
      scrollOffsetY = nextY;
      scrollRef?.scrollTo?.({ y: nextY, animated: false });
    }
    autoScrollFrame = requestAnimationFrame(autoScrollStep);
  };

  const updateAutoScroll = (active: boolean, pointerY: number) => {
    if (!active || scrollViewportHeight <= 0) {
      stopAutoScroll();
      return;
    }
    const topEdge = scrollViewportTop + AUTOSCROLL_EDGE;
    const bottomEdge = scrollViewportTop + scrollViewportHeight - AUTOSCROLL_EDGE;
    let dir = 0;
    if (pointerY < topEdge) dir = -1;
    else if (pointerY > bottomEdge) dir = 1;
    if (dir === 0) {
      stopAutoScroll();
      return;
    }
    autoScrollDir = dir;
    if (autoScrollFrame == null) {
      autoScrollLast = performance.now();
      autoScrollFrame = requestAnimationFrame(autoScrollStep);
    }
  };

  cleanup(stopAutoScroll);

  const registerRowRef = (key: string, el: any) => {
    if (el) rowRefs.set(key, el);
    else rowRefs.delete(key);
  };

  const revealRow = (key: string) => {
    setTimeout(() => {
      rowRefs.get(key)?.reveal?.({ vertical: "nearest", animated: true });
    }, 0);
  };

  const recomputeOffscreen = () => {
    const sb = list.sidebar;
    const sel = selected_session(list.selection);
    const notices = sb.notices_besides(sel);
    if (notices.length === 0 || !scrollRef?.measure) {
      if (offscreenAbove().length > 0) offscreenAbove.set([]);
      if (offscreenBelow().length > 0) offscreenBelow.set([]);
      return;
    }
    const serial = ++measureSerial;
    scrollRef
      .measure()
      .then((box: any) => {
        if (serial !== measureSerial || !box) return;
        scrollViewportTop = box.pagePosition?.y ?? scrollViewportTop;
        scrollViewportHeight = box.bounds?.size?.height ?? scrollViewportHeight;
        const vpTop = scrollViewportTop;
        const vpBottom = vpTop + scrollViewportHeight;
        return Promise.all(
          notices.map((notice: any) => {
            const el = rowRefs.get(notice.key);
            if (!el?.measure) return Promise.resolve({ key: notice.key, box: null });
            return el.measure().then((m: any) => ({ key: notice.key, box: m })).catch(() => ({
              key: notice.key,
              box: null,
            }));
          }),
        ).then((measured: any[]) => {
          if (serial !== measureSerial) return;
          const above: string[] = [];
          const below: string[] = [];
          for (const item of measured) {
            if (!item.box) continue;
            const y = item.box.pagePosition?.y ?? 0;
            const h = item.box.bounds?.size?.height ?? ROW_MD;
            if (y + h <= vpTop + 2) above.push(item.key);
            else if (y >= vpBottom - 2) below.push(item.key);
          }
          offscreenAbove.set(above);
          offscreenBelow.set(below);
        });
      })
      .catch(() => {});
  };

  const refreshMachinesMenu = () => {
    fetchMachinesMenuGroups(list.sidebar.machines).then((groups) => {
      machinesMenuGroups.set(groups);
    });
  };

  effect(() => {
    const unsub = registerFocusCallback(() => {
      rootRef?.focus?.();
    });
    cleanup(unsub);
  });

  effect(() => {
    machineMenuVersion();
    refreshMachinesMenu();
  });

  // 跟踪侧栏与选中变化：驱动 FLIP 布局弹簧动画、新落进/定位铺底、刚开跑图标提亮、自动滚入可视区与离屏胶囊。
  effect(() => {
    tick();
    const sb = list.sidebar;
    const selected = selected_session(list.selection);
    if (searching() && list.project_filter === "" && sb.filtering === false) {
      // 如果外部清掉了筛选词（比如从通知跳转），保持与 list.project_filter 一致。
    }
    if (!same_conversation(selected, seenSelected)) {
      seenSelected = selected;
      if (selected) revealRow(session_flip(selected));
    }
    if (sb !== seenSidebar) {
      const prevGen = listMotion.generation;
      seenSidebar = sb;
      const now = Date.now();
      listMotion.track(sb, now);
      if (listMotion.generation !== prevGen) {
        listContentRef?.animateLayout?.({ type: "spring", stiffness: 320, damping: 32, mass: 1 });
      }
      if (list.fresh && sb.has_session(list.fresh)) {
        const key = session_flip(list.fresh);
        listMotion.arrive(key, now);
        list.fresh = null;
        revealRow(key);
      }
      if (list.located) {
        const key = session_flip(list.located);
        list.located = null;
        listMotion.locate(key, now);
        revealRow(key);
      }
      motionTick.set(motionTick() + 1);
      recomputeOffscreen();
    } else if (list.located) {
      const key = session_flip(list.located);
      list.located = null;
      listMotion.locate(key, Date.now());
      motionTick.set(motionTick() + 1);
      revealRow(key);
    }
  });

  const motionAccess: MotionAccess = {
    washSerial(key: string) {
      motionTick();
      return listMotion.wash_serial(key);
    },
    glowSerial(key: string) {
      motionTick();
      return listMotion.glow_serial(key);
    },
  };

  const look = () => theme();
  const sidebar = () => {
    tick();
    return list.sidebar;
  };
  const showAvatar = computed((): boolean => {
    tick();
    return show_avatar(list.avatar);
  });
  const hasConversations = computed(() => sidebar().conversations != null);
  const conversationsCollapsed = computed(() => sidebar().conversations?.collapsed ?? false);
  const byProject = computed(() => sidebar().grouping === "project");
  const hasRemote = computed(() => sidebar().has_remote());
  const waitingMachines = computed(() =>
    sidebar().by_machine ? [] : sidebar().machines.filter((m: any) => !m.received),
  );
  const groupingActive = computed(
    () =>
      sidebar().grouping !== "project" ||
      sidebar().hide_empty ||
      sidebar().by_machine,
  );
  const headerButtonsPinned = computed(
    () => groupingOpen() || addProjectOpen() || machinesMenuOpen() || groupingActive(),
  );
  const offscreen = computed(() => {
    const sb = sidebar();
    const sel = selected_session(list.selection);
    return hint(sb.notices_besides(sel), offscreenAbove(), offscreenBelow());
  });

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

  const consumeSuppressedPress = () => Date.now() < suppressPressUntil;

  const beginDrag = (
    info: Omit<DragState, "active" | "overKey" | "overPayload" | "before" | "lastTargetY">,
  ) => {
    hitValidTargetInMove = false;
    drag.set({
      ...info,
      active: false,
      overKey: null,
      overPayload: null,
      before: true,
      lastTargetY: info.startY,
    });
    if (scrollRef?.measure) {
      scrollRef
        .measure()
        .then((box: any) => {
          if (box) {
            scrollViewportTop = box.pagePosition?.y ?? scrollViewportTop;
            scrollViewportHeight = box.bounds?.size?.height ?? scrollViewportHeight;
          }
        })
        .catch(() => {});
    }
  };

  const hoverDrag = (
    dragKind: string | null,
    key: string,
    payload: any,
    localY: number,
    rowHeight: number,
    pageY: number,
  ) => {
    const cur = drag();
    if (!cur) return;
    if (!dragKind || dragKind !== cur.dragKind) {
      hitValidTargetInMove = false;
      if (cur.overKey != null) {
        drag.set({ ...cur, overKey: null, overPayload: null });
      }
      return;
    }
    hitValidTargetInMove = true;
    const active = cur.active || Math.abs(pageY - cur.startY) >= DRAG_THRESHOLD;
    if (key === cur.key) {
      if (cur.active !== active || cur.overKey != null || cur.pointerY !== pageY) {
        drag.set({
          ...cur,
          active,
          pointerY: pageY,
          overKey: null,
          overPayload: null,
          lastTargetY: pageY,
        });
      }
      return;
    }
    const before = localY < rowHeight / 2;
    if (
      cur.active !== active ||
      cur.overKey !== key ||
      cur.before !== before ||
      cur.pointerY !== pageY
    ) {
      drag.set({
        ...cur,
        active,
        pointerY: pageY,
        overKey: key,
        overPayload: payload,
        before,
        lastTargetY: pageY,
      });
    }
  };

  const handleRootMouseMove = (e: any) => {
    const cur = drag();
    if (!cur) return;
    if (e?.pressedButton !== "left") {
      stopAutoScroll();
      drag.set(null);
      return;
    }
    const pageY = e?.pagePosition?.y ?? cur.pointerY;
    const active = cur.active || Math.abs(pageY - cur.startY) >= DRAG_THRESHOLD;
    const keepOver = hitValidTargetInMove || Math.abs(pageY - cur.lastTargetY) <= 8;
    hitValidTargetInMove = false;
    if (cur.active !== active || cur.pointerY !== pageY || (!keepOver && cur.overKey != null)) {
      drag.set({
        ...cur,
        active,
        pointerY: pageY,
        overKey: keepOver ? cur.overKey : null,
        overPayload: keepOver ? cur.overPayload : null,
      });
    }
    updateAutoScroll(active, pageY);
  };

  const finishDrag = () => {
    stopAutoScroll();
    const cur = drag();
    if (!cur) return;
    drag.set(null);
    if (!cur.active) return;
    // 拖拽超过阈值后，松手不再触发行点击（打开会话 / 选中项目）。
    suppressPressUntil = Date.now() + 250;
    if (!cur.overPayload || cur.overKey == null || cur.overKey === cur.key) return;
    listMotion.drop_at(cur.key, cur.pointerY - cur.grabOffsetY);
    listContentRef?.animateLayout?.({ type: "spring", stiffness: 320, damping: 32, mass: 1 });
    if (cur.kind === "project") {
      actions.drop_project(
        list,
        { machine: cur.payload.machine, root: cur.payload.root },
        { machine: cur.overPayload.machine, root: cur.overPayload.root },
        cur.before,
      );
    } else if (cur.kind === "session" && cur.payload.group?.kind === "project") {
      actions.drop_session(
        list,
        cur.payload.key,
        cur.overPayload.key,
        cur.payload.group,
        cur.before,
      );
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

  const dragPending = computed(() => drag() != null);
  const dragSessionPending = computed(() => drag()?.kind === "session");
  const dragProjectPending = computed(() => drag()?.kind === "project");

  const listVisible = computed(() => viewShown(LIST));
  const sharedRowProps: SharedRowProps = {
    sidebar,
    showAvatar,
    visible: listVisible,
    rename,
    startRename,
    updateRename,
    commitRename,
    cancelRename,
    ask,
    drag,
    dragSessionPending,
    dragProjectPending,
    beginDrag,
    hoverDrag,
    consumeSuppressedPress,
    motion: motionAccess,
    registerRowRef,
  };

  const addProjectButton = () => (
    <If
      when={hasRemote()}
      else={
        <View
          role="button"
          accessibilityLabel={t("list.add_project")}
          tooltip={<Tip text={t("list.add_project")} />}
          class="items-center justify-center rounded-(--radius) hover:bg-accent"
          style={{ width: 24, height: 24, cursor: "default" }}
          onPress={() => actions.open_project(list, null)}
        >
          <SmeltIcon path={icon("folder-plus")} size={14} color={look().palette.muted} />
        </View>
      }
    >
      <SmeltMenu
        placement="bottom"
        align="end"
        popupStyle={{ minWidth: 176 }}
        onOpenChange={({ open }) => addProjectOpen.set(open)}
        trigger={
          <View
            role="button"
            accessibilityLabel={t("list.add_project")}
            tooltip={<Tip text={t("list.add_project")} />}
            class={`items-center justify-center rounded-(--radius) ${
              addProjectOpen() ? "bg-accent" : "hover:bg-accent"
            }`}
            style={{ width: 24, height: 24, cursor: "default" }}
          >
            <SmeltIcon path={icon("folder-plus")} size={14} color={look().palette.muted} />
          </View>
        }
      >
        <MenuSectionTitle text={t("menu.open_on_machine")} />
        <Each of={sidebar().machines} key={(m: any) => machine_flip(m.machine)}>
          {(m: () => any) => (
            <MenuEntry
              label={m().name}
              iconPath={m().os_icon}
              iconColor={lightColor(look().palette, m().link?.light ?? "good")}
              disabled={m().link?.unavailable != null}
              onSelect={() => actions.open_project(list, m().machine)}
            />
          )}
        </Each>
      </SmeltMenu>
    </If>
  );

  const projectsHeaderRow = (
    <View
      class="group/header flex-row items-center"
      style={{ height: ROW_SM, paddingLeft: space(2), marginTop: space(1), gap: space(1) }}
    >
      <If
        when={searching()}
        else={
          <View
            role="button"
            testID="search-projects-trigger"
            tooltip={<Tip text={t("list.search_projects")} />}
            class="group/search flex-1 min-w-0 flex-row items-center rounded-(--radius) hover:bg-background"
            hoverStyle={{ borderWidth: 1, borderColor: look().palette.hair_strong }}
            style={{
              height: 24,
              marginLeft: -6,
              paddingHorizontal: 6,
              gap: space(1.5),
              cursor: "text",
            }}
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
            <View class="opacity-0 group-hover/search:opacity-100" style={{ flexShrink: 0 }}>
              <SmeltIcon path={icon("search")} size={12} color={look().palette.muted} />
            </View>
          </View>
        }
      >
        <ProjectSearchInput
          value={() => {
            tick();
            return list.project_filter;
          }}
          onChange={(value) => list.set_project_filter(value)}
          onSubmit={() => rootRef?.focus?.()}
          onClose={() => searching.set(false)}
          onClearAndClose={() => {
            searching.set(false);
            list.set_project_filter("");
          }}
        />
      </If>

      <View
        class={
          headerButtonsPinned()
            ? "flex-row items-center opacity-100"
            : "flex-row items-center opacity-0 group-hover/header:opacity-100"
        }
        style={{ flexShrink: 0 }}
      >
        <GroupingMenu onOpenChange={(open) => groupingOpen.set(open)} />
        {addProjectButton()}
        <If when={machinesMenuGroups().length > 0}>
          <SmeltMenu
            placement="bottom"
            align="end"
            popupStyle={{ minWidth: 176 }}
            onOpenChange={({ open }) => {
              machinesMenuOpen.set(open);
              if (open) refreshMachinesMenu();
            }}
            trigger={
              <View
                role="button"
                accessibilityLabel={t("list.machine")}
                tooltip={<Tip text={t("list.machine")} />}
                class={`items-center justify-center rounded-(--radius) ${
                  machinesMenuOpen() ? "bg-accent" : "hover:bg-accent"
                }`}
                style={{ width: 24, height: 24, cursor: "default" }}
              >
                <SmeltIcon path={icon("ellipsis")} size={14} color={look().palette.muted} />
              </View>
            }
          >
            <MachinesMenuEntries groups={machinesMenuGroups()} hasRemote={hasRemote()} />
          </SmeltMenu>
        </If>
      </View>
    </View>
  );

  return (
    <View
      ref={(el: any) => {
        rootRef = el;
      }}
      testID="session-list-root"
      focusable={true}
      style={{
        flex: 1,
        minHeight: 0,
        width: "100%",
        height: "100%",
        display: "flex",
        flexDirection: "column",
      }}
      onMouseMove={dragPending() ? handleRootMouseMove : undefined}
      onMouseUp={dragPending() ? finishDrag : undefined}
      onFileDrop={(e: any) => {
        const uris = Array.isArray(e?.uris) ? e.uris : [];
        const paths = uris.filter((uri: string) => uri.length > 0).map(fileUriToPath);
        if (paths.length > 0) {
          actions.open_dropped_paths(list, paths);
        }
      }}
      onKeyDown={(e: any) => {
        if (e?.key === "Escape") {
          if (drag()) {
            stopAutoScroll();
            drag.set(null);
            return;
          }
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
        if ((e?.modifiers?.platform || e?.modifiers?.control) && e?.key === "ArrowUp") {
          list.step_session(false).catch(() => {});
        } else if (
          (e?.modifiers?.platform || e?.modifiers?.control) &&
          e?.key === "ArrowDown"
        ) {
          list.step_session(true).catch(() => {});
        }
      }}
    >
      <SmeltWindowBar view={LIST} />

      <View style={{ position: "relative", flex: 1, minHeight: 0, width: "100%" }}>
        <Scroll
          ref={(el: any) => {
            scrollRef = el;
          }}
          style={{ flex: 1, minHeight: 0, width: "100%", height: "100%" }}
          onLayout={({ bounds }: any) => {
            if (bounds?.size) {
              scrollViewportHeight = bounds.size.height;
              recomputeOffscreen();
            }
          }}
          onScroll={(e: any) => {
            scrollOffsetY = e?.offset?.y ?? scrollOffsetY;
            scrollViewportHeight = e?.size?.height ?? scrollViewportHeight;
            scrollContentHeight = e?.contentSize?.height ?? scrollContentHeight;
            recomputeOffscreen();
          }}
        >
          <View
            ref={(el: any) => {
              listContentRef = el;
            }}
            style={{ paddingHorizontal: space(2), paddingBottom: space(3), gap: space(1) }}
          >
            {/* 「对话」段 */}
            <If when={hasConversations()}>
              <View style={{ paddingBottom: space(1) }}>
                <SmeltDisclosure
                  open={!conversationsCollapsed()}
                 
                  content={
                    <View style={{ gap: 2, paddingTop: 2 }}>
                      <Each
                        of={sidebar().conversations.rows}
                        key={(row: any) => session_flip(row.key)}
                      >
                        {(row: () => any) => (
                          <SessionRow
                            row={row}
                            level={0}
                            {...sharedRowProps}
                            visible={() => listVisible() && !conversationsCollapsed()}
                          />
                        )}
                      </Each>
                    </View>
                  }
                >
                  <View
                    ref={(el: any) => registerRowRef(CONVERSATIONS_KEY, el)}
                    role="button"
                    class="group/row flex-row items-center hover:bg-accent"
                    style={{
                      height: ROW_SM,
                      paddingHorizontal: space(2),
                      gap: space(2),
                      borderRadius: look().radius.row,
                      cursor: "default",
                    }}
                    onPress={() =>
                      list.set_collapsed(
                        { kind: "conversations" },
                        !sidebar().conversations.collapsed,
                      )
                    }
                  >
                    <DisclosureSlot
                      baseIcon={icon("message-square")}
                      foldKey={CONVERSATIONS_KEY}
                      collapsed={conversationsCollapsed()}
                      collapsible={true}
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
                    <If when={conversationsCollapsed()}>
                      <CollapsedSummary
                        status={sidebar().conversations.status}
                        count={sidebar().conversations.rows.length}
                      />
                    </If>
                  </View>
                </SmeltDisclosure>
              </View>
            </If>

            {/* 「项目」标题栏（悬停搜索入口 + 分组菜单 + 添加项目 + 机器菜单；右键开机器菜单） */}
            <If
              when={machinesMenuGroups().length > 0}
              else={projectsHeaderRow}
            >
              <SmeltContextMenu
                popupStyle={{ minWidth: 184 }}
                onOpenChange={({ open }) => {
                  if (open) refreshMachinesMenu();
                }}
                trigger={projectsHeaderRow}
              >
                <MachinesMenuEntries groups={machinesMenuGroups()} hasRemote={hasRemote()} />
              </SmeltContextMenu>
            </If>

            {/* 区域：按机器或整份 */}
            <Each
              of={sidebar().areas}
              key={(area: any) =>
                area.machine === undefined ? "all" : machine_flip(area.machine)
              }
            >
              {(area: () => any) => {
                const machineInfo = () =>
                  area().machine === undefined
                    ? null
                    : sidebar().machines.find((m: any) => m.machine === area().machine) ?? null;
                const areaCollapsed = computed(() => area().collapsed);
                const areaVisible = computed(
                  () => listVisible() && (machineInfo() == null || !areaCollapsed()),
                );
                const bucketLevel = () => (sidebar().by_machine ? 1 : 0);

                const areaBody = (
                  <View style={{ gap: 2, paddingTop: machineInfo() != null ? 2 : 0 }}>
                    <If
                      when={byProject()}
                      else={
                        <>
                          <Each of={area().buckets} key={(group: any) => bucket_flip(group.key)}>
                            {(group: () => any) => {
                              const bKey = () => bucket_flip(group().key);
                              const bucketCollapsed = computed(() => group().collapsed);
                              const bucketVisible = computed(
                                () => areaVisible() && !bucketCollapsed(),
                              );
                              return (
                                <SmeltDisclosure
                                  open={!bucketCollapsed()}
                                 
                                  content={
                                    <View style={{ gap: 2, paddingTop: 2 }}>
                                      <Each
                                        of={group().rows}
                                        key={(row: any) => session_flip(row.key)}
                                      >
                                        {(row: () => any) => (
                                          <SessionRow
                                            row={row}
                                            level={bucketLevel() + 1}
                                            {...sharedRowProps}
                                            visible={bucketVisible}
                                          />
                                        )}
                                      </Each>
                                    </View>
                                  }
                                >
                                  <View
                                    ref={(el: any) => registerRowRef(bKey(), el)}
                                    role="button"
                                    class="group/row flex-row items-center hover:bg-accent"
                                    style={{
                                      height: ROW_MD,
                                      paddingLeft: indent(bucketLevel()),
                                      paddingRight: space(1),
                                      gap: space(2),
                                      borderRadius: look().radius.row,
                                      cursor: "default",
                                    }}
                                    onPress={() =>
                                      list.set_bucket_collapsed(bKey(), !group().collapsed)
                                    }
                                  >
                                    <DisclosureSlot
                                      baseIcon={icon(bucketCollapsed() ? "folder" : "folder-open")}
                                      foldKey={bKey()}
                                      collapsed={bucketCollapsed()}
                                      collapsible={true}
                                    />
                                    <Text
                                      class="flex-1 min-w-0"
                                      style={{
                                        color: look().palette.text,
                                        fontSize: look().type.ui.size,
                                        lineHeight: look().type.ui.line_height,
                                        numberOfLines: 1,
                                      }}
                                    >
                                      {bucket_label(group().key.bucket, t)}
                                    </Text>
                                    <If when={bucketCollapsed()}>
                                      <CollapsedSummary
                                        status={group().status}
                                        count={group().rows.length}
                                      />
                                    </If>
                                  </View>
                                </SmeltDisclosure>
                              );
                            }}
                          </Each>
                          <If when={area().buckets.length === 0}>
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
                          </If>
                        </>
                      }
                    >
                      <>
                        <Each
                          of={visibleProjectsOf(area())}
                          key={(project: any) => project_flip(project.machine, project.root)}
                        >
                          {(project: () => any) => (
                            <ProjectSection
                              project={project}
                              byMachine={sidebar().by_machine}
                              {...sharedRowProps}
                              visible={areaVisible}
                            />
                          )}
                        </Each>

                        <If when={hiddenCountOf(area()) > 0}>
                          <View
                            role="button"
                            class="flex-row items-center rounded-(--radius) hover:bg-accent"
                            style={{ height: ROW_SM, paddingHorizontal: space(2), cursor: "default" }}
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
                        </If>

                        <If
                          when={
                            visibleProjectsOf(area()).length === 0 && hiddenCountOf(area()) === 0
                          }
                        >
                          <View style={{ paddingHorizontal: space(2), paddingVertical: space(3) }}>
                            <Text
                              style={{
                                color: look().palette.faint,
                                fontSize: look().type.text_xsmall.size,
                                lineHeight: look().type.text_xsmall.line_height,
                              }}
                            >
                              {sidebar().filtering
                                ? t("list.no_matching_projects")
                                : t("list.no_projects")}
                            </Text>
                          </View>
                        </If>
                      </>
                    </If>
                  </View>
                );

                return (
                  <If when={machineInfo() != null} else={areaBody}>
                    <SmeltDisclosure open={!areaCollapsed()} content={areaBody}>
                      <MachineHeaderRow
                        machine={() => machineInfo()!}
                        area={area}
                        registerRowRef={registerRowRef}
                      />
                    </SmeltDisclosure>
                  </If>
                );
              }}
            </Each>

            {/* 不按机器分开时，还没收到工作区目录的机器垫在底部 */}
            <Each of={waitingMachines()} key={(m: any) => machine_flip(m.machine)}>
              {(m: () => any) => <WaitingMachineRow machine={m} />}
            </Each>
          </View>
        </Scroll>

        <OffscreenPill side={() => offscreen().above} up={true} />
        <OffscreenPill side={() => offscreen().below} up={false} />
      </View>

      {/* 拖拽悬浮预览浮层：跨滚动裁切显示在窗口顶层，完全穿透指针。 */}
      <If when={drag()?.active === true}>
        <Overlay style={{ pointerEvents: "none" }}>
          <DragPreviewCard drag={drag} />
        </Overlay>
      </If>

      <ConfirmDialog request={confirm} onDismiss={() => confirm.set(null)} />
    </View>
  );
}

/** 插件无头根：承载新建会话面板和项目仓库对话框。 */
export function root() {
  return (
    <>
      <PaletteDialog />
      <RepositoriesDialog />
    </>
  );
}

export const settings = SessionListSettings;
export default main;
