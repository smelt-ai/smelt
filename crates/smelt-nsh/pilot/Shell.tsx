// 试点的外壳：按方案 2 摆的一块 NSH 界面，嵌在 smelt 的停靠区里（`smelt-nsh`）。插件只按 id 留格子
// （`Slot`），各自一包，装、改、删都不动这里。
// 左边顶上和底下是启动栏插件的两个视图，中间是本机的对话（每行一个小人插件的头像，另留一个转圈
// 量整块重画）；右边上半是点中的那段对话，下半是新开的终端，都是宿主登记的视图，作原生组件
// `smelt.view` 嵌进来。「菜单」是 TSX 画的弹层，试它盖不盖得住终端。

import { host, nativeComponent, nativeModule, Scroll, signal, Text, View } from "nsh";
import { Popover } from "nsh/base";

import { Slot } from "./slots.tsx";

interface Conversation { id: string; conversation: string; title: string }

/** `smelt/pilot` 的 `Smelt`（`smelt-nsh` 的 `src/module.rs`）。 */
interface SmeltApi {
  /** 本机现在的对话，之后每变一次再给一次；返回停止监听的函数。 */
  on(event: "conversations", listener: (list: Conversation[]) => void): () => void;
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

// 没人听的事件会一直排着：外壳先听着，插件后装上时自己 `host.call("state")` 取眼下这份。
host.on("smelt.state", () => {});

const conversations = signal<Conversation[]>([]);
Smelt.on("conversations", (list) => conversations.set(list));

const MENU = [
  { id: "first", title: "第一项" },
  { id: "second", title: "第二项" },
  { id: "third", title: "第三项" },
];

export function Shell() {
  const picked = signal("");
  const terminal = signal("");
  const menu = signal(false);
  const chosen = signal("");

  return (
    <View class="flex-1 flex-row">
      <View class="border-r border-border" style={{ width: 260 }}>
        <Slot id="launcher" view="Nav" args={{ edges: ["top", "left"] }} />
        <View class="flex-row items-center gap-2 px-3 py-2">
          <View class="size-3 rounded-full border-2 border-foreground border-t-transparent animate-spin" />
          <Text class="flex-1 text-sm font-medium text-foreground">对话</Text>
        </View>
        <Scroll style={{ flex: 1 }}>
          <View class="gap-0.5 px-2 pb-2">
            <Each of={conversations()} key={(conversation: Conversation) => conversation.id}>
              {(conversation: () => Conversation) => (
                <View
                  role="button"
                  onPress={() => picked.set(conversation().id)}
                  class={`flex-row items-center gap-2 rounded-(--radius) px-2 py-1 active:bg-accent ${
                    picked() === conversation().id ? "bg-accent" : ""
                  }`}
                >
                  <Slot id="buddies" args={{ machine: null, conversation: conversation().conversation }} />
                  <Text class="flex-1 min-w-0 text-sm text-foreground" style={{ numberOfLines: 1 }}>
                    {conversation().title}
                  </Text>
                </View>
              )}
            </Each>
          </View>
        </Scroll>
        <View class="py-1.5">
          <Slot id="launcher" view="Settings" args={{ corner: "bottom-left" }} />
        </View>
      </View>
      <View class="flex-1">
        <View class="flex-1">
          {picked() ? (
            <SmeltView
              spec={{ view: "smelt.conversation/conversation", args: { machine: null, session_id: picked() } }}
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
