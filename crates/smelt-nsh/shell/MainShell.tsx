// Smelt 主窗口 NSH 外壳（SMELT_SHELL=nsh）：
// - 单引擎线程 + 单界面树（Core）承载全部页面、DockArea、浮动的左右停靠区开关与插件无头根；
// - 每个面板通过外壳专用的 `<HostView>` 挂载（JS 插件走内部 `<Slot>`，Rust 视图走 `smelt.view`）；
// - 不含任何具体功能或插件名称，完全按宿主推送的 `smelt.state.shell` 驱动。
import { cleanup, computed, effect, host, signal, Text, View } from "nsh";
import { DockArea } from "nsh/base";
import { Slot } from "smelt/slots";
import {
  SmeltBackdropRegion,
  SmeltButton,
  SmeltNativeView,
  SmeltNativeWindowBar,
  SmeltNativeWindowDrag,
  stableMemo,
  state,
} from "smelt/sdk";

/** 外壳专用视图挂载：JS 插件走内部 `<Slot>`，宿主内置/Rust 视图走 `<SmeltNativeView>`。 */
function HostView(props: {
  view: string;
  args?: unknown;
  class?: string;
  style?: any;
}) {
  const jsTargetKey = computed(() => {
    const target = state().js_views?.[props.view];
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

interface ShellSnapshot {
  padding: number;
  pages: PageSnapshot[];
  settings_page: { id: string; title: string } | null;
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
    const pages = (state().shell as ShellSnapshot | undefined)?.pages ?? [];
    return pages.find((p) => p.key === props.pageKey)?.outlet ?? null;
  });
  const outletArea = stableMemo(() => state().safe_areas?.["smelt/outlet"] ?? { title_bar: false });
  // 用纯字符串区分出口挂载方式：
  // - 单例 JS 视图（如会话页）：同视图切参数时字符串不变，绝不卸载重建；
  // - 按实例建的 JS 视图（如远程桌面）：每次打开（seq 递增）按语义建新实例。
  const outletMode = computed(() => {
    const o = outlet();
    if (!o) return outletArea().title_bar ? "empty-bar" : "empty";
    if (o.managed_rust_instance) return "managed";
    if (state().js_views?.[o.id]) {
      const isInstance = Boolean(state().views?.find((v: any) => v.id === o.id)?.instances);
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
    () => (state().shell as ShellSnapshot | undefined)?.settings_page ?? null,
  );
  const settingsId = computed(() => settingsPage()?.id ?? "");
  const settingsTitle = computed(() => settingsPage()?.title ?? "");
  const area = stableMemo(
    () =>
      state().safe_areas?.["smelt/settings"] ?? {
        title_bar: false,
        top_bar_insets: { left: 0, right: 0 },
      },
  );
  const theme = stableMemo(() => state().theme);
  const emptyText = computed(
    () =>
      (state().shell as ShellSnapshot | undefined)?.labels?.settings_page_gone ??
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
    const pages = (state().shell as ShellSnapshot | undefined)?.pages ?? [];
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
    const pages = (state().shell as ShellSnapshot | undefined)?.pages ?? [];
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
  const theme = stableMemo(() => state().theme);
  const labels = stableMemo(() => (state().shell as ShellSnapshot | undefined)?.labels);
  const padding = computed(() => (state().shell as ShellSnapshot | undefined)?.padding ?? 0);

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
            renderTabBarLeading={(group: any) => {
              const area = page()?.group_areas?.[String(group.id)];
              const left = area?.top_bar_insets?.left ?? 0;
              if (left <= 0) return null;
              return (
                <View style={{ width: left, height: TOP_BAR_HEIGHT, flexShrink: 0 }}>
                  {area?.title_bar ? (
                    <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
                  ) : null}
                </View>
              );
            }}
            renderTab={(tab: any) => {
              const meta = page()?.meta?.[tab.panel];
              const title = meta?.title ?? tab.panel;
              const badge = meta?.badge;
              const active = Boolean(tab.active);
              const tStyle = active ? theme().type.ui_medium : theme().type.ui;
              return (
                <View
                  style={{
                    height: TOP_BAR_HEIGHT - 8,
                    paddingLeft: 8,
                    paddingRight: 8,
                    marginLeft: 2,
                    marginRight: 2,
                    borderRadius: theme().radius.row,
                    backgroundColor: active ? theme().palette.row_selected : "transparent",
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 4,
                  }}
                >
                  <Text
                    style={{
                      fontSize: tStyle.size,
                      lineHeight: tStyle.line_height,
                      fontWeight: tStyle.weight,
                      color: active ? theme().palette.text : theme().palette.muted,
                      numberOfLines: 1,
                    }}
                  >
                    {title}
                  </Text>
                  {badge ? (
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
                        {badge}
                      </Text>
                    </View>
                  ) : null}
                </View>
              );
            }}
            renderTabBarTrailing={(group: any) => {
              const area = page()?.group_areas?.[String(group.id)];
              const right = area?.top_bar_insets?.right ?? 0;
              const activeMeta = group.active ? page()?.meta?.[group.active] : null;
              const zoomed = Boolean(group.zoomed);
              const canZoom = zoomed || (activeMeta && !activeMeta.draws_top_bar && activeMeta.kind === "view");
              const lbl = labels();
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
                    {area?.title_bar ? (
                      <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
                    ) : null}
                  </View>
                  {canZoom ? (
                    <View style={{ flexShrink: 0, paddingRight: 4 }}>
                      <SmeltButton
                        quiet={true}
                        small={true}
                        selected={zoomed}
                        toggled={zoomed}
                        icon={zoomed ? "icons/minimize.svg" : "icons/maximize.svg"}
                        tooltip={zoomed ? lbl?.restore ?? "Restore" : lbl?.zoom ?? "Zoom"}
                        onPress={() => {
                          if (dockRef) dockRef.toggleZoom(group.id);
                        }}
                      />
                    </View>
                  ) : null}
                  {right > 0 ? (
                    <View style={{ width: right, height: TOP_BAR_HEIGHT, flexShrink: 0 }}>
                      {area?.title_bar ? (
                        <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
                      ) : null}
                    </View>
                  ) : null}
                </View>
              );
            }}
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

export function Shell() {
  const pageKeys = stableMemo(() =>
    ((state().shell as ShellSnapshot | undefined)?.pages ?? []).map((p) => p.key),
  );
  const pluginRoots = stableMemo(
    () => (state().shell as ShellSnapshot | undefined)?.plugin_roots ?? [],
  );

  return (
    <View style={{ position: "relative", width: "100%", height: "100%", flex: 1, minWidth: 0, minHeight: 0 }}>
      <Each of={pageKeys()} key={(key: string) => key}>
        {(key: () => string) => <PageWorkspace pageKey={key()} />}
      </Each>

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
