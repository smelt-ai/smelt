// 启动栏：plugins/launcher 在 NSH 试点里的移植，行为照它的 main.js。算什么在 model.js，原样拷来的。
// 试点里由外壳按 id 摆，所以不登记视图。

import { host, nativeComponent, signal, Text, View } from "nsh";

import { NEW_CONVERSATION, SETTINGS, nav_state, same_state, settings_state } from "./model.js";

/** 行高（smelt-ui 的 height::md）。 */
const ROW_MD = 32;

const Button = nativeComponent<
  { label: string; icon?: string; quiet?: boolean; fill?: boolean; tooltip?: string; command?: string },
  { press: {} }
>("smelt.button", {
  props: ["label", "icon", "quiet", "fill", "tooltip", "command"],
  events: ["press"],
});
const Icon = nativeComponent<{ path: string; color: string }, {}>("smelt.icon", {
  props: ["path", "color"],
  events: [],
});
const WindowBar = nativeComponent<{ edges: string[] }, {}>("smelt.window-bar", {
  props: ["edges"],
  events: [],
});

const state = signal<any>(null);
const nav = signal<any>({ new_conversation: null, entries: [] });
const settings = signal<any>({ available: false, attention: false });

/** Tailwind 的一档间距（0.25rem）按界面字号折成点：原插件的间距跟着字号走。 */
function space(steps: number): number {
  return ((state()?.theme.rem ?? 16) * steps) / 4;
}

function t(key: string): string {
  const now = state();
  return now?.texts[now.locale]?.[key] ?? now?.texts.en?.[key] ?? key;
}

function refresh(now: any) {
  const views = new Map<string, any>(now.views.map((view: any) => [view.id, view]));
  const model = {
    pages: () => now.pages,
    views: () => now.views,
    command: (id: string) => now.commands.find((command: any) => command.id === id) ?? null,
    view_badge: (id: string) => views.get(id)?.badge ?? null,
    view_laid_out: (id: string) => views.get(id)?.laid_out ?? false,
    view_shown: (id: string) => views.get(id)?.shown ?? false,
  };
  const next_nav = nav_state(model);
  if (!same_state(next_nav, nav())) nav.set(next_nav);
  const next_settings = settings_state(model);
  if (!same_state(next_settings, settings())) settings.set(next_settings);
}

// 文案只在第一次的回话里（按插件各给各的），推来的状态里没有。
let texts = {};
let revision = -1;
function receive(next: any) {
  if (next.texts) texts = next.texts;
  if (next.revision > revision) {
    revision = next.revision;
    state.set({ ...next, texts });
    refresh(next);
  } else if (next.texts && state()) {
    state.set({ ...state(), texts });
  }
}
host.on("smelt.state", receive);
host.call("state").then(receive, notify_error);

function notify_error(error: any) {
  const message = error?.message ?? String(error);
  console.error(`[launcher] ${message}`);
  host
    .call("run", { command: "smelt.app/notify", args: { subtitle: t("notify.error"), body: message } })
    .catch(() => {});
}

function open(view: string) {
  host.call("run", { command: "smelt.app/open-view", args: { view } }).catch(notify_error);
}

function run(command: string) {
  host.call("run", { command }).catch(notify_error);
}

function EntryRow(props: { entry: () => any }) {
  const entry = props.entry;
  const look = () => state().theme;
  return (
    <View
      role="button"
      onPress={() => open(entry().id)}
      class={`flex-row items-center ${
        entry().shown ? "bg-sidebar-accent" : "hover:bg-accent active:bg-sidebar-accent"
      }`}
      style={{
        height: ROW_MD,
        gap: space(2),
        paddingLeft: space(2),
        paddingRight: space(2),
        borderRadius: look().radius.row,
        cursor: "default",
      }}
    >
      <View style={{ width: 16, height: 16, flexShrink: 0 }}>
        <Icon
          path={entry().icon}
          color={entry().shown ? look().palette.muted_selected : look().palette.muted}
          style={{ width: 16, height: 16 }}
        />
        {/* 原生组件自己吃指针；它只是画，盖一层，点击和悬停才算这一行的。 */}
        <View style={{ position: "absolute", top: 0, left: 0, width: 16, height: 16 }} />
      </View>
      <Text
        class="flex-1 min-w-0 text-foreground"
        style={{
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          fontWeight: entry().shown ? look().type.ui_medium.weight : look().type.ui.weight,
          numberOfLines: 1,
        }}
      >
        {entry().title}
      </Text>
      {entry().attention ? (
        <View style={{ width: 6, height: 6, borderRadius: 3, flexShrink: 0, backgroundColor: look().palette.bad }} />
      ) : null}
    </View>
  );
}

export function Nav(props: { args?: any }) {
  return (
    <View class="w-full min-w-0">
      <WindowBar edges={props.args?.edges ?? []} class="w-full" />
      <View
        class="w-full min-w-0"
        style={{
          paddingLeft: space(2),
          paddingRight: space(2),
          paddingTop: space(2),
          paddingBottom: space(1),
          gap: space(1),
        }}
      >
        {nav().new_conversation != null ? (
          <Button
            label={nav().new_conversation}
            icon="icons/plus.svg"
            fill={true}
            tooltip={nav().new_conversation}
            command={NEW_CONVERSATION}
            onPress={() => run(NEW_CONVERSATION)}
            class="w-full min-w-0"
          />
        ) : null}
        <Each of={nav().entries} key={(entry: any) => entry.id}>
          {(entry: () => any) => <EntryRow entry={entry} />}
        </Each>
      </View>
    </View>
  );
}

export function Settings(props: { args?: any }) {
  const insets = () => state()?.bar_insets[props.args?.corner ?? "none"] ?? { left: 0, right: 0 };
  return (
    <View class="w-full min-w-0 flex-row items-center" style={{ paddingLeft: insets().left, paddingRight: insets().right }}>
      {settings().available ? (
        <View style={{ flexShrink: 0 }}>
          <Button label={t("settings")} icon="icons/settings.svg" quiet={true} onPress={() => run(SETTINGS)} />
          {settings().attention ? (
            <View
              style={{
                position: "absolute",
                top: 4,
                right: 4,
                width: 5,
                height: 5,
                borderRadius: 2.5,
                pointerEvents: "none",
                backgroundColor: state().theme.palette.bad,
              }}
            />
          ) : null}
        </View>
      ) : null}
    </View>
  );
}
