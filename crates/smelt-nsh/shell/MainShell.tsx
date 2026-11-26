// Smelt 主窗口 NSH 外壳（SMELT_SHELL=nsh）：
// - 单引擎线程 + 单界面树（Core）承载全部页面、DockArea、浮动的左右停靠区开关与插件无头根；
// - 每个面板通过外壳专用的 `<HostView>` 挂载（JS 插件走内部 `<Slot>`，Rust 视图走 `smelt.view`）；
// - 不含任何具体功能或插件名称，完全按宿主推送的 `smelt.state.shell` 驱动。
import { cleanup, computed, effect, host, Scroll, signal, Text, View } from "nsh";
import { Button as BaseButton, DockArea } from "nsh/base";
import { Slot } from "smelt/slots";
import {
  DIALOG_SURFACE_CLASS,
  initHost,
  jsViews,
  safeArea,
  shellState,
  SmeltBackdropRegion,
  SmeltButton,
  SmeltDialog,
  SmeltIcon,
  SmeltNativeView,
  SmeltNativeWindowBar,
  SmeltNativeWindowDrag,
  stableMemo,
  theme as currentTheme,
  views,
} from "smelt/sdk";

void initHost(["locale", "theme", "shell", "js_views", "views", "pages", "safe_areas"]);

/** 外壳专用视图挂载：JS 插件走内部 `<Slot>`，宿主内置/Rust 视图走 `<SmeltNativeView>`。 */
function HostView(props: {
  view: string;
  args?: unknown;
  class?: string;
  style?: any;
}) {
  const jsTargetKey = computed(() => {
    const target = jsViews()?.[props.view];
    return target ? `${target.plugin}\0${target.view}` : "";
  });
  const jsPlugin = computed(() => {
    const key = jsTargetKey();
    const sep = key.indexOf("\0");
    return sep >= 0 ? key.slice(0, sep) : "";
  });
  const jsView = computed(() => {
    const key = jsTargetKey();
    const sep = key.indexOf("\0");
    return sep >= 0 ? key.slice(sep + 1) : "default";
  });
  const nativeSpec = stableMemo(() => ({
    view: props.view,
    args: props.args ?? null,
  }));

  return (
    <View
      class={props.class}
      style={{
        display: "flex",
        flexDirection: "column",
        minWidth: 0,
        minHeight: 0,
        ...(props.style ?? {}),
      }}
    >
      <If
        when={jsTargetKey() !== ""}
        else={
          <SmeltNativeView
            spec={nativeSpec()}
            style={{ flex: 1, minWidth: 0, minHeight: 0 }}
          />
        }
      >
        <Slot
          id={jsPlugin()}
          view={jsView()}
          args={props.args}
          style={{ flex: 1, width: "100%", height: "100%", minWidth: 0, minHeight: 0 }}
        />
      </If>
    </View>
  );
}

const TOP_BAR_HEIGHT = 34;
const DOCK_TOGGLE_WIDTH = 20;
const WINDOW_EDGE_MARGIN = 18;

interface StackNodeSpec {
  axis: "horizontal" | "vertical";
  items: Array<{
    view?: string;
    stack?: StackNodeSpec;
    fit?: boolean;
  }>;
}

interface PageSnapshot {
  key: string;
  active: boolean;
  layout_rev?: number;
  layout: any;
  panels: any[];
  meta: Record<string, any>;
  group_areas: Record<string, any>;
  outlet: { id: string; args: any; seq: number; managed_rust_instance?: boolean } | null;
  left_dock: boolean | null;
  right_dock: boolean | null;
  left_toggle_x: number;
  zoomed: number | null;
}

interface QuitDialogSnapshot {
  title: string;
  body: string;
  cancel_label: string;
  confirm_label: string;
  quitting: boolean;
}

interface DirPickerEntrySnapshot {
  name: string;
  path: string;
  is_dir: boolean;
  selected: boolean;
}

interface DirPickerDialogSnapshot {
  title: string;
  current_path: string | null;
  can_go_up: boolean;
  loading: boolean;
  error: string | null;
  entries: DirPickerEntrySnapshot[];
  multiple: boolean;
  selected_summary: string | null;
  confirm_label: string;
  can_confirm: boolean;
  cancel_label: string;
  up_tooltip: string;
  home_label: string;
  loading_label: string;
  empty_label: string;
  select_tooltip: string;
  deselect_tooltip: string;
  list_rev: number;
}

interface HostDialogsSnapshot {
  quit: QuitDialogSnapshot | null;
  dir_picker: DirPickerDialogSnapshot | null;
}

interface ShellSnapshot {
  padding: number;
  pages: PageSnapshot[];
  settings_page: { id: string; title: string } | null;
  host_dialogs?: HostDialogsSnapshot;
  plugin_roots: string[];
  labels: {
    collapse_left: string;
    expand_left: string;
    collapse_right: string;
    expand_right: string;
    zoom: string;
    restore: string;
    settings_page_gone: string;
  };
}

function StackView(props: { node: StackNodeSpec }) {
  const vertical = props.node.axis === "vertical";
  return (
    <View
      style={{
        display: "flex",
        flexDirection: vertical ? "column" : "row",
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
      }}
    >
      {props.node.items.map((item) => {
        const fit = Boolean(item.fit);
        const itemStyle = vertical
          ? fit
            ? { flexShrink: 0, flexGrow: 0, width: "100%", display: "flex", flexDirection: "column", minWidth: 0, minHeight: 0 }
            : { flex: 1, width: "100%", display: "flex", flexDirection: "column", minWidth: 0, minHeight: 0 }
          : fit
            ? { flexShrink: 0, flexGrow: 0, height: "100%", display: "flex", flexDirection: "column", minWidth: 0, minHeight: 0 }
            : { flex: 1, height: "100%", display: "flex", flexDirection: "column", minWidth: 0, minHeight: 0 };
        return (
          <View style={itemStyle}>
            {item.view ? (
              <HostView view={item.view} style={{ flex: 1, width: "100%", height: "100%" }} />
            ) : item.stack ? (
              <StackView node={item.stack} />
            ) : null}
          </View>
        );
      })}
    </View>
  );
}

function OutletPanel(props: { pageKey: string }) {
  const outlet = computed(() => {
    const pages = (shellState() as ShellSnapshot | undefined)?.pages ?? [];
    return pages.find((p) => p.key === props.pageKey)?.outlet ?? null;
  });
  const outletArea = stableMemo(() => safeArea("smelt/outlet"));
  // 用纯字符串区分出口挂载方式：
  // - 单例 JS 视图（如会话页）：同视图切参数时字符串不变，绝不卸载重建；
  // - 按实例建的 JS 视图（如远程桌面）：每次打开（seq 递增）按语义建新实例。
  const outletMode = computed(() => {
    const o = outlet();
    if (!o) return outletArea().title_bar ? "empty-bar" : "empty";
    if (o.managed_rust_instance) return "managed";
    if (jsViews()?.[o.id]) {
      const isInstance = Boolean(views()?.find((v: any) => v.id === o.id)?.instances);
      return isInstance ? `js-inst:${o.id}:${o.seq}` : `js:${o.id}`;
    }
    return `rust:${o.id}`;
  });
  const managedSpec = { view: "smelt/outlet" };
  const rustSpec = stableMemo(() => ({ view: outlet()?.id ?? "" }));

  return (
    <View style={{ display: "flex", flexDirection: "column", flex: 1, width: "100%", height: "100%", minWidth: 0, minHeight: 0 }}>
      {(() => {
        const mode = outletMode();
        if (mode === "managed") {
          return (
            <SmeltNativeView
              spec={managedSpec}
              style={{ flex: 1, width: "100%", height: "100%" }}
            />
          );
        }
        if (mode.startsWith("js-inst:")) {
          const rest = mode.slice(8);
          const id = rest.slice(0, rest.lastIndexOf(":"));
          return (
            <HostView
              view={id}
              args={outlet()?.args}
              style={{ flex: 1, width: "100%", height: "100%" }}
            />
          );
        }
        if (mode.startsWith("js:")) {
          const id = mode.slice(3);
          return (
            <HostView
              view={id}
              args={outlet()?.args}
              style={{ flex: 1, width: "100%", height: "100%" }}
            />
          );
        }
        if (mode.startsWith("rust:")) {
          return (
            <SmeltNativeView
              spec={rustSpec()}
              style={{ flex: 1, width: "100%", height: "100%" }}
            />
          );
        }
        if (mode === "empty-bar") {
          return <SmeltNativeWindowBar safeArea={outletArea()} style={{ width: "100%", flexShrink: 0 }} />;
        }
        return null;
      })()}
    </View>
  );
}

function SettingsContentPanel() {
  const settingsPage = computed(
    () => (shellState() as ShellSnapshot | undefined)?.settings_page ?? null,
  );
  const settingsId = computed(() => settingsPage()?.id ?? "");
  const settingsTitle = computed(() => settingsPage()?.title ?? "");
  const area = stableMemo(() => safeArea("smelt/settings") as any);
  const theme = currentTheme;
  const emptyText = computed(
    () =>
      (shellState() as ShellSnapshot | undefined)?.labels?.settings_page_gone ??
      "Settings page unavailable",
  );

  return (
    <View style={{ display: "flex", flexDirection: "column", flex: 1, width: "100%", height: "100%", minWidth: 0, minHeight: 0 }}>
      <View
        style={{
          position: "relative",
          width: "100%",
          flexShrink: 0,
          height: area().title_bar ? TOP_BAR_HEIGHT : undefined,
          paddingLeft: (area().top_bar_insets?.left ?? 0) + 20,
          paddingRight: (area().top_bar_insets?.right ?? 0) + 20,
          paddingTop: area().title_bar ? 0 : 8,
          paddingBottom: area().title_bar ? 0 : 8,
          borderBottomWidth: 1,
          borderBottomColor: theme().palette.hair,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
        }}
      >
        <If when={area().title_bar}>
          <View style={{ position: "absolute", top: 0, left: 0, right: 0, bottom: 0 }}>
            <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
          </View>
        </If>
        <If when={settingsId() !== ""}>
          <Text
            style={{
              fontSize: theme().type.ui_medium.size,
              lineHeight: theme().type.ui_medium.line_height,
              fontWeight: theme().type.ui_medium.weight,
              color: theme().palette.text,
              pointerEvents: "none",
              numberOfLines: 1,
            }}
          >
            {settingsTitle()}
          </Text>
        </If>
      </View>
      <View style={{ flex: 1, minHeight: 0, minWidth: 0, display: "flex", flexDirection: "column" }}>
        <If
          when={settingsId() !== ""}
          else={
            <View style={{ padding: 24 }}>
              <Text style={{ fontSize: theme().type.ui.size, color: theme().palette.muted }}>
                {emptyText()}
              </Text>
            </View>
          }
        >
          <HostView view={settingsId()} style={{ flex: 1, width: "100%", height: "100%" }} />
        </If>
      </View>
    </View>
  );
}

function PanelBody(props: { pageKey: string; panelId: string }) {
  // 只挑结构字段并按内容去重：仅切换会话或更新标题/角标时绝不重挂面板内容树。
  const bodySpec = stableMemo(() => {
    const pages = (shellState() as ShellSnapshot | undefined)?.pages ?? [];
    const m = pages.find((p) => p.key === props.pageKey)?.meta?.[props.panelId] ?? null;
    if (!m || m.kind === "placeholder") return { kind: "placeholder" as const };
    if (m.kind === "stack" && m.stack) return { kind: "stack" as const, stack: m.stack as StackNodeSpec };
    return { kind: "view" as const, view: String(m.view ?? "") };
  });
  return (
    <View style={{ display: "flex", flexDirection: "column", flex: 1, width: "100%", height: "100%", minWidth: 0, minHeight: 0 }}>
      {(() => {
        const spec = bodySpec();
        if (spec.kind === "placeholder") return null;
        if (spec.kind === "stack") return <StackView node={spec.stack} />;
        if (spec.view === "smelt/outlet") return <OutletPanel pageKey={props.pageKey} />;
        if (spec.view === "smelt/settings") return <SettingsContentPanel />;
        return <HostView view={spec.view} style={{ flex: 1, width: "100%", height: "100%" }} />;
      })()}
    </View>
  );
}

function TabBarLeading(props: { pageKey: string; groupId: string }) {
  const area = stableMemo(() => {
    const pages = (shellState() as ShellSnapshot | undefined)?.pages ?? [];
    const entry = pages.find((p) => p.key === props.pageKey)?.group_areas?.[props.groupId];
    return {
      left: Number(entry?.top_bar_insets?.left ?? 0),
      titleBar: Boolean(entry?.title_bar),
    };
  });
  return (
    <If when={area().left > 0}>
      <View style={{ width: area().left, height: TOP_BAR_HEIGHT, flexShrink: 0 }}>
        <If when={area().titleBar}>
          <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
        </If>
      </View>
    </If>
  );
}

function TabItem(props: { pageKey: string; panelId: string; active: boolean }) {
  const theme = currentTheme;
  const tabMeta = stableMemo(() => {
    const pages = (shellState() as ShellSnapshot | undefined)?.pages ?? [];
    const meta = pages.find((p) => p.key === props.pageKey)?.meta?.[props.panelId];
    const title = typeof meta?.title === "string" && meta.title.length > 0 ? meta.title : props.panelId;
    const badge = typeof meta?.badge === "string" && meta.badge.length > 0 ? meta.badge : null;
    return { title, badge };
  });
  const tStyle = computed(() => (props.active ? theme().type.ui_medium : theme().type.ui));

  return (
    <View
      style={{
        height: TOP_BAR_HEIGHT - 8,
        paddingLeft: 8,
        paddingRight: 8,
        marginLeft: 2,
        marginRight: 2,
        borderRadius: theme().radius.row,
        backgroundColor: props.active ? theme().palette.row_selected : "transparent",
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        gap: 4,
      }}
    >
      <Text
        style={{
          fontSize: tStyle().size,
          lineHeight: tStyle().line_height,
          fontWeight: tStyle().weight,
          color: props.active ? theme().palette.text : theme().palette.muted,
          numberOfLines: 1,
        }}
      >
        {tabMeta().title}
      </Text>
      <If when={tabMeta().badge !== null}>
        <View
          style={{
            paddingLeft: 5,
            paddingRight: 5,
            height: 16,
            borderRadius: 8,
            backgroundColor: theme().palette.soft,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
          }}
        >
          <Text
            style={{
              fontSize: theme().type.text_xsmall.size,
              color: theme().palette.text,
            }}
          >
            {tabMeta().badge ?? ""}
          </Text>
        </View>
      </If>
    </View>
  );
}

function TabBarTrailing(props: {
  pageKey: string;
  groupId: string;
  activePanel: string | null;
  zoomed: boolean;
  onToggleZoom: () => void;
}) {
  const trailing = stableMemo(() => {
    const snap = shellState() as ShellSnapshot | undefined;
    const currentPage = (snap?.pages ?? []).find((p) => p.key === props.pageKey);
    const area = currentPage?.group_areas?.[props.groupId];
    const right = Number(area?.top_bar_insets?.right ?? 0);
    const titleBar = Boolean(area?.title_bar);
    const activeMeta = props.activePanel ? currentPage?.meta?.[props.activePanel] : null;
    const canZoom =
      props.zoomed ||
      Boolean(activeMeta && !activeMeta.draws_top_bar && activeMeta.kind === "view");
    const lbl = snap?.labels;
    return {
      right,
      titleBar,
      canZoom,
      zoomLabel: lbl?.zoom ?? "Zoom",
      restoreLabel: lbl?.restore ?? "Restore",
    };
  });

  return (
    <View
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        width: "100%",
        height: TOP_BAR_HEIGHT,
      }}
    >
      <View style={{ flex: 1, height: "100%", minWidth: 8 }}>
        <If when={trailing().titleBar}>
          <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
        </If>
      </View>
      <If when={trailing().canZoom}>
        <View style={{ flexShrink: 0, paddingRight: 4 }}>
          <SmeltButton
            quiet={true}
            small={true}
            selected={props.zoomed}
            toggled={props.zoomed}
            icon={props.zoomed ? "icons/minimize.svg" : "icons/maximize.svg"}
            tooltip={props.zoomed ? trailing().restoreLabel : trailing().zoomLabel}
            onPress={props.onToggleZoom}
          />
        </View>
      </If>
      <If when={trailing().right > 0}>
        <View style={{ width: trailing().right, height: TOP_BAR_HEIGHT, flexShrink: 0 }}>
          <If when={trailing().titleBar}>
            <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
          </If>
        </View>
      </If>
    </View>
  );
}

function PageWorkspace(props: { pageKey: string }) {
  let dockRef: any = null;
  effect(() => {
    const unsub = host.on("shell.unzoom", (data: any) => {
      if (data?.page === props.pageKey && dockRef) {
        dockRef.toggleZoom(null);
      }
    });
    cleanup(() => {
      if (typeof unsub === "function") unsub();
    });
  });
  const page = computed(() => {
    const pages = (shellState() as ShellSnapshot | undefined)?.pages ?? [];
    return pages.find((p) => p.key === props.pageKey) ?? null;
  });
  const hasPage = computed(() => page() !== null);
  const pageActive = computed(() => Boolean(page()?.active));
  // 本地乐观布局：用户点击开合停靠区或拖动分隔条时当帧直接生效，不等宿主往返与存盘；
  // 宿主主动重装/显露视图时 `layout_rev` 递增，自然覆盖本地缓存。
  const localLayout = signal<{ rev: number; layout: any } | null>(null);
  const dockLayout = stableMemo(() => {
    const p = page();
    if (!p) return null;
    const loc = localLayout();
    if (loc && loc.rev === (p.layout_rev ?? 0)) return loc.layout;
    return p.layout ?? null;
  });
  const dockPanels = stableMemo(() => page()?.panels ?? []);
  const hasLeftDock = computed(() => page()?.left_dock != null);
  const leftDockOpen = computed(() => {
    const left = dockLayout()?.left;
    if (left && typeof left.open === "boolean") return left.open;
    return Boolean(page()?.left_dock);
  });
  const leftToggleX = computed(() => page()?.left_toggle_x ?? WINDOW_EDGE_MARGIN);
  const hasRightDock = computed(() => page()?.right_dock != null);
  const rightDockOpen = computed(() => {
    const right = dockLayout()?.right;
    if (right && typeof right.open === "boolean") return right.open;
    return Boolean(page()?.right_dock);
  });
  const theme = currentTheme;
  const labels = stableMemo(() => (shellState() as ShellSnapshot | undefined)?.labels);
  const padding = computed(() => (shellState() as ShellSnapshot | undefined)?.padding ?? 0);

  const reportLayout = (event: { layout?: any } | any) => {
    const nextLayout = event?.layout ?? event;
    if (!nextLayout) return;
    localLayout.set({ rev: page()?.layout_rev ?? 0, layout: nextLayout });
    host
      .call("shell.layout_changed", { page: props.pageKey, layout: nextLayout })
      .catch(() => {});
  };
  const reportZoom = (event: { panel?: string | null } | string | null) => {
    const panel = typeof event === "object" && event !== null ? event.panel ?? null : event;
    const group = dockRef && panel ? dockRef.groupOf(panel) : null;
    host
      .call("shell.zoom_changed", { page: props.pageKey, group: group ?? null })
      .catch(() => {});
  };

  return (
    <View
      style={{
        position: "absolute",
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        display: pageActive() ? "flex" : "none",
        flexDirection: "column",
        padding: padding(),
      }}
    >
      <If when={hasPage()}>
        <View style={{ position: "relative", flex: 1, minHeight: 0, minWidth: 0, display: "flex" }}>
          <DockArea
            ref={(api: any) => {
              dockRef = api;
            }}
            style={{ flex: 1, width: "100%", height: "100%" }}
            compact={false}
            layout={dockLayout()}
            panels={dockPanels()}
            onLayoutChange={reportLayout}
            onZoomChange={reportZoom}
            tabBarStyle={{
              flexDirection: "row",
              alignItems: "center",
              height: TOP_BAR_HEIGHT,
              flexShrink: 0,
              borderBottomWidth: 1,
              borderBottomColor: theme().palette.hair,
            }}
            handleStyle={{
              backgroundColor: theme().palette.hair,
            }}
            dropIndicatorStyle={{
              backgroundColor: theme().palette.row_selected,
              borderWidth: 1,
              borderColor: theme().palette.focus,
              borderRadius: theme().radius.md,
            }}
            renderRegionBackdrop={(placement: string) => (
              <SmeltBackdropRegion
                placement={placement}
                style={{ width: "100%", height: "100%", pointerEvents: "none" }}
              />
            )}
            renderTabBarLeading={(group: any) => (
              <TabBarLeading pageKey={props.pageKey} groupId={String(group.id)} />
            )}
            renderTab={(tab: any) => (
              <TabItem pageKey={props.pageKey} panelId={tab.panel} active={Boolean(tab.active)} />
            )}
            renderTabBarTrailing={(group: any) => (
              <TabBarTrailing
                pageKey={props.pageKey}
                groupId={String(group.id)}
                activePanel={group.active ?? null}
                zoomed={Boolean(group.zoomed)}
                onToggleZoom={() => {
                  if (dockRef) dockRef.toggleZoom(group.id);
                }}
              />
            )}
            renderPanel={(panel: any) => (
              <PanelBody pageKey={props.pageKey} panelId={panel.id} />
            )}
          />

          {/* 左右停靠区浮动开合按钮，与主壳原位对齐 */}
          <If when={hasLeftDock()}>
            <View
              style={{
                position: "absolute",
                top: 0,
                left: leftToggleX(),
                width: DOCK_TOGGLE_WIDTH,
                height: TOP_BAR_HEIGHT,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <SmeltButton
                quiet={true}
                small={true}
                icon={leftDockOpen() ? "icons/panel-left-filled.svg" : "icons/panel-left.svg"}
                tooltip={
                  leftDockOpen()
                    ? labels()?.collapse_left ?? "Collapse left sidebar"
                    : labels()?.expand_left ?? "Expand left sidebar"
                }
                onPress={() => {
                  if (dockRef) dockRef.toggleDock("left");
                }}
              />
            </View>
          </If>

          <If when={hasRightDock()}>
            <View
              style={{
                position: "absolute",
                top: 0,
                right: WINDOW_EDGE_MARGIN,
                width: DOCK_TOGGLE_WIDTH,
                height: TOP_BAR_HEIGHT,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <SmeltButton
                quiet={true}
                small={true}
                icon={rightDockOpen() ? "icons/panel-right-filled.svg" : "icons/panel-right.svg"}
                tooltip={
                  rightDockOpen()
                    ? labels()?.collapse_right ?? "Collapse right sidebar"
                    : labels()?.expand_right ?? "Expand right sidebar"
                }
                onPress={() => {
                  if (dockRef) dockRef.toggleDock("right");
                }}
              />
            </View>
          </If>
        </View>
      </If>
    </View>
  );
}

function sendHostDialogAction(kind: string, path?: string) {
  host
    .call("shell.host_dialog_action", path === undefined ? { kind } : { kind, path })
    .catch(() => {});
}

function HostDialogs() {
  const theme = currentTheme;
  const dialogs = computed(() => (shellState() as ShellSnapshot | undefined)?.host_dialogs);
  const quit = computed(() => dialogs()?.quit ?? null);
  const picker = computed(() => dialogs()?.dir_picker ?? null);

  let dirScrollRef: any = null;
  effect(() => {
    const rev = picker()?.list_rev ?? 0;
    if (rev > 0) {
      dirScrollRef?.scrollTo?.({ y: 0, animated: false });
    }
  });

  const dirBodyKind = computed(() => {
    const p = picker();
    if (!p) return "none";
    if (p.error != null) return "error";
    if (p.current_path == null) return "loading";
    if (p.entries.length === 0) return "empty";
    return "list";
  });

  return (
    <>
      <SmeltDialog
        open={quit() != null}
        role="alertdialog"
        closeOnBackdropPress={false}
        closeOnEscape={!quit()?.quitting}
        accessibilityLabel={quit()?.title ?? ""}
        accessibilityDescription={quit()?.body ?? ""}
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open && !quit()?.quitting) sendHostDialogAction("quit_cancel");
        }}
        class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
        style={{ width: 440, gap: 12 }}
      >
        <If when={quit() != null}>
          <Text
            style={{
              fontSize: theme().type.ui_medium.size,
              lineHeight: theme().type.ui_medium.line_height,
              fontWeight: theme().type.ui_medium.weight,
              color: theme().palette.text,
            }}
          >
            {quit()?.title ?? ""}
          </Text>
          <Text
            style={{
              fontSize: theme().type.ui.size,
              lineHeight: theme().type.ui.line_height,
              color: theme().palette.muted,
            }}
          >
            {quit()?.body ?? ""}
          </Text>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              disabled={Boolean(quit()?.quitting)}
              label={quit()?.cancel_label ?? ""}
              onPress={() => sendHostDialogAction("quit_cancel")}
            />
            <SmeltButton
              primary={true}
              small={true}
              loading={Boolean(quit()?.quitting)}
              disabled={Boolean(quit()?.quitting)}
              label={quit()?.confirm_label ?? ""}
              onPress={() => sendHostDialogAction("quit_confirm")}
            />
          </View>
        </If>
      </SmeltDialog>

      <SmeltDialog
        open={picker() != null}
        role="dialog"
        closeOnBackdropPress={false}
        closeOnEscape={true}
        accessibilityLabel={picker()?.title ?? ""}
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) sendHostDialogAction("dir_picker_cancel");
        }}
        class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
        style={{ width: 520, gap: 12 }}
      >
        <If when={picker() != null}>
          <Text
            style={{
              fontSize: theme().type.ui_medium.size,
              lineHeight: theme().type.ui_medium.line_height,
              fontWeight: theme().type.ui_medium.weight,
              color: theme().palette.text,
            }}
          >
            {picker()?.title ?? ""}
          </Text>

          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              icon="icons/arrow-up.svg"
              tooltip={picker()?.up_tooltip ?? ""}
              disabled={!picker()?.can_go_up}
              onPress={() => sendHostDialogAction("dir_picker_up")}
            />
            <SmeltButton
              quiet={true}
              small={true}
              label={picker()?.home_label ?? ""}
              onPress={() => sendHostDialogAction("dir_picker_home")}
            />
            <Text
              style={{
                flex: 1,
                minWidth: 0,
                fontSize: theme().type.text_xsmall.size,
                lineHeight: theme().type.text_xsmall.line_height,
                color: theme().palette.muted,
                numberOfLines: 1,
              }}
            >
              {picker()?.current_path ?? "…"}
            </Text>
          </View>

          <If when={dirBodyKind() === "loading"}>
            <View style={{ paddingTop: 24, paddingBottom: 24 }}>
              <Text
                style={{
                  fontSize: theme().type.ui.size,
                  lineHeight: theme().type.ui.line_height,
                  color: theme().palette.muted,
                }}
              >
                {picker()?.loading_label ?? ""}
              </Text>
            </View>
          </If>

          <If when={dirBodyKind() === "error"}>
            <View style={{ paddingTop: 24, paddingBottom: 24 }}>
              <Text
                style={{
                  fontSize: theme().type.ui.size,
                  lineHeight: theme().type.ui.line_height,
                  color: theme().palette.bad,
                }}
              >
                {picker()?.error ?? ""}
              </Text>
            </View>
          </If>

          <If when={dirBodyKind() === "empty"}>
            <View style={{ paddingTop: 24, paddingBottom: 24 }}>
              <Text
                style={{
                  fontSize: theme().type.ui.size,
                  lineHeight: theme().type.ui.line_height,
                  color: theme().palette.muted,
                }}
              >
                {picker()?.empty_label ?? ""}
              </Text>
            </View>
          </If>

          <If when={dirBodyKind() === "list"}>
            <View
              style={{
                height: 320,
                display: "flex",
                flexDirection: "column",
                overflow: "hidden",
                borderWidth: 1,
                borderColor: theme().palette.hair,
                borderRadius: theme().radius.row,
              }}
            >
              <Scroll
                ref={(api: any) => {
                  dirScrollRef = api;
                }}
                style={{ flex: 1, width: "100%", height: "100%" }}
              >
                <View style={{ display: "flex", flexDirection: "column", width: "100%" }}>
                  <Each of={picker()?.entries ?? []} key={(e: DirPickerEntrySnapshot) => e.path}>
                    {(entry: () => DirPickerEntrySnapshot) => (
                      <View
                        style={{
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          width: "100%",
                          backgroundColor: entry().selected
                            ? theme().palette.row_selected
                            : "transparent",
                        }}
                      >
                        <BaseButton
                          onPress={() => {
                            const item = entry();
                            sendHostDialogAction(
                              item.is_dir ? "dir_picker_navigate" : "dir_picker_toggle",
                              item.path,
                            );
                          }}
                          style={{
                            flex: 1,
                            minWidth: 0,
                            display: "flex",
                            flexDirection: "row",
                            alignItems: "center",
                            gap: 8,
                            paddingLeft: 12,
                            paddingRight: 12,
                            paddingTop: 6,
                            paddingBottom: 6,
                            backgroundColor: "transparent",
                            cursor: "default",
                          }}
                          hoverStyle={{
                            backgroundColor: entry().selected
                              ? theme().palette.row_selected
                              : theme().palette.row_hover,
                          }}
                          activeStyle={{
                            backgroundColor: theme().palette.row_selected,
                          }}
                        >
                          <SmeltIcon
                            path={entry().is_dir ? "icons/folder.svg" : "icons/file.svg"}
                            size={14}
                            color={theme().palette.muted}
                          />
                          <Text
                            style={{
                              flex: 1,
                              minWidth: 0,
                              fontSize: theme().type.ui.size,
                              lineHeight: theme().type.ui.line_height,
                              color: theme().palette.text,
                              numberOfLines: 1,
                            }}
                          >
                            {entry().name}
                          </Text>
                        </BaseButton>
                        <If when={Boolean(picker()?.multiple && entry().is_dir)}>
                          <View style={{ flexShrink: 0, paddingRight: 8 }}>
                            <SmeltButton
                              quiet={true}
                              small={true}
                              icon={entry().selected ? "icons/check.svg" : "icons/plus.svg"}
                              tooltip={
                                entry().selected
                                  ? picker()?.deselect_tooltip ?? ""
                                  : picker()?.select_tooltip ?? ""
                              }
                              onPress={() => sendHostDialogAction("dir_picker_toggle", entry().path)}
                            />
                          </View>
                        </If>
                      </View>
                    )}
                  </Each>
                </View>
              </Scroll>
            </View>
          </If>

          <If when={picker()?.selected_summary != null}>
            <Text
              style={{
                fontSize: theme().type.text_xsmall.size,
                lineHeight: theme().type.text_xsmall.line_height,
                color: theme().palette.muted,
              }}
            >
              {picker()?.selected_summary ?? ""}
            </Text>
          </If>

          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={picker()?.cancel_label ?? ""}
              onPress={() => sendHostDialogAction("dir_picker_cancel")}
            />
            <SmeltButton
              primary={true}
              small={true}
              disabled={!picker()?.can_confirm}
              label={picker()?.confirm_label ?? ""}
              onPress={() => sendHostDialogAction("dir_picker_confirm")}
            />
          </View>
        </If>
      </SmeltDialog>
    </>
  );
}

export function Shell() {
  const pageKeys = stableMemo(() =>
    ((shellState() as ShellSnapshot | undefined)?.pages ?? []).map((p) => p.key),
  );
  const pluginRoots = stableMemo(
    () => (shellState() as ShellSnapshot | undefined)?.plugin_roots ?? [],
  );

  // 主题到之前不画：页面都按主题取色，窗口底由宿主铺。
  return (
    <View style={{ position: "relative", width: "100%", height: "100%", flex: 1, minWidth: 0, minHeight: 0 }}>
      <Each of={currentTheme() ? pageKeys() : []} key={(key: string) => key}>
        {(key: () => string) => <PageWorkspace pageKey={key()} />}
      </Each>

      <If when={Boolean(currentTheme())}>
        <HostDialogs />
      </If>

      {/* 插件无头根：不占空间，承载命令弹出的对话框（⌘N、仓库管理）及后台事件循环 */}
      <View
        style={{
          position: "absolute",
          top: 0,
          left: 0,
          width: 0,
          height: 0,
          overflow: "hidden",
        }}
      >
        <Each of={pluginRoots()} key={(id: string) => id}>
          {(id: () => string) => <Slot id={id()} view="root" />}
        </Each>
      </View>
    </View>
  );
}
