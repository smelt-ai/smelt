// 试点的外壳：按方案 2 摆的一块 NSH 界面，嵌在 smelt 的停靠区里（`smelt-nsh`）。插件只按 id 留格子
// （`Slot`），各自跑在独立 QuickJS VM 里，装、改、删都不动这里。
// 左边顶上和底下是启动栏插件的两个视图，中间是独立的会话列表插件（它内部按跨 VM 插槽嵌小人插件）；
// 右边上半是当前选中的对话，下半是新开的终端，都是宿主登记的视图，作原生组件 `smelt.view` 嵌进来。
// 「菜单」是 TSX 画的弹层，试它盖不盖得住终端。

import { host, nativeComponent, nativeModule, signal, Text, View } from "nsh";
import { Popover } from "nsh/base";
import { Slot } from "smelt/slots";

interface SmeltApi {
  /** 新的守护会话 id。 */
  newSessionId(): string;
}

const { Smelt } = nativeModule<{ Smelt: SmeltApi }>("smelt/pilot");

/** 宿主登记的视图：`spec` 给视图 id 和参数；`ends` 是这一格新开的终端会话，放掉时结束它。 */
const SmeltView = nativeComponent<{ spec: { view: string; args: unknown }; ends?: string }, {}>(
  "smelt.view",
  { props: ["spec", "ends"], events: [] },
);

/** smelt-ui 的按钮，宿主登记的 GPUI 组件。 */
const SmeltButton = nativeComponent<{ label: string; small?: boolean }, { press: {} }>("smelt.button", {
  props: ["label", "small"],
  events: ["press"],
});

interface ActiveSession {
  machine: string | null;
  id: string;
}

const activeSession = signal<ActiveSession | null>(null);
let stateRevision = -1;

function receiveState(next: any) {
  if (!next || typeof next.revision !== "number" || next.revision <= stateRevision) return;
  stateRevision = next.revision;
  const session = next.active_context?.session;
  if (session && typeof session.id === "string") {
    const current = activeSession();
    if (!current || current.id !== session.id || current.machine !== (session.machine ?? null)) {
      activeSession.set({ machine: session.machine ?? null, id: session.id });
    }
  } else if (activeSession() !== null) {
    activeSession.set(null);
  }
}

host.on("smelt.state", receiveState);
host.call("state").then(receiveState, () => {});

const MENU = [
  { id: "first", title: "第一项" },
  { id: "second", title: "第二项" },
  { id: "third", title: "第三项" },
];

export function Shell() {
  const terminal = signal("");
  const menu = signal(false);
  const chosen = signal("");

  return (
    <View class="flex-1 flex-row">
      <View class="border-r border-border flex-col" style={{ width: 260 }}>
        <Slot id="launcher" view="Nav" args={{ edges: ["top", "left"] }} />
        <Slot
          id="session-list"
          args={{ avatarPlugin: "buddies" }}
          style={{ flex: 1, minHeight: 0 }}
        />
        <View class="py-1.5">
          <Slot id="launcher" view="Settings" args={{ corner: "bottom-left" }} />
        </View>
      </View>
      <View class="flex-1">
        <View class="flex-1">
          {activeSession() ? (
            <SmeltView
              spec={{
                view: "smelt.conversation/conversation",
                args: { machine: activeSession()!.machine, session_id: activeSession()!.id },
              }}
              style={{ flex: 1 }}
            />
          ) : (
            <View class="flex-1 items-center justify-center">
              <Text class="text-sm text-muted-foreground">点左边一段对话</Text>
            </View>
          )}
        </View>
        <View class="flex-row items-center gap-2 border-t border-border px-2 py-1.5">
          <SmeltButton
            small={true}
            label={terminal() ? "关终端" : "开终端"}
            onPress={() => terminal.set(terminal() ? "" : Smelt.newSessionId())}
          />
          <Popover
            open={menu()}
            onOpenChange={(next: { open?: boolean }) => menu.set(next.open ?? !menu())}
            align="start"
            trigger={
              <SmeltButton
                small={true}
                label={chosen() ? `菜单：${chosen()}` : "菜单"}
                onPress={() => menu.set(!menu())}
              />
            }
          >
            <View class="rounded-(--radius) border border-border bg-popover p-1" style={{ width: 160 }}>
              <Each of={MENU} key={(item: { id: string }) => item.id}>
                {(item: () => { id: string; title: string }) => (
                  <View
                    role="button"
                    onPress={() => {
                      chosen.set(item().title);
                      menu.set(false);
                    }}
                    class="justify-center rounded-(--radius) px-2 hover:bg-accent"
                    style={{ height: 32 }}
                  >
                    <Text class="text-sm text-popover-foreground">{item().title}</Text>
                  </View>
                )}
              </Each>
              <SmeltButton small={true} label="关菜单" onPress={() => menu.set(false)} />
            </View>
          </Popover>
        </View>
        {terminal() ? (
          <SmeltView
            spec={{ view: "smelt.terminal/terminal", args: { machine: null, session_id: terminal() } }}
            ends={terminal()}
            style={{ flex: 1 }}
          />
        ) : null}
      </View>
    </View>
  );
}
