// 试点：按方案 2 摆的一块 NSH 界面，嵌在 smelt 的停靠区里（`smelt-nsh`）。
// 左边本机的对话列表，顶上一直转圈（每帧都有动画，量整块重画）；右边上半是点中的那段对话，
// 下半是新开的终端，都是宿主登记的视图，作原生组件 `smelt.view` 嵌进来。

import { mount, nativeComponent, nativeModule, Scroll, signal, Text, View } from "nsh";

interface Conversation { id: string; title: string }

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
const SmeltButton = nativeComponent<{ label: string }, { press: {} }>("smelt.button", {
  props: ["label"],
  events: ["press"],
});

const conversations = signal<Conversation[]>([]);
Smelt.on("conversations", (list) => conversations.set(list));

function App() {
  const picked = signal("");
  const terminal = signal("");

  return (
    <View class="flex-1 flex-row">
      <View class="border-r border-border" style={{ width: 260 }}>
        <View class="flex-row items-center gap-2 px-3 py-2">
          <View class="size-3 rounded-full border-2 border-foreground border-t-transparent animate-spin" />
          <Text class="flex-1 text-sm font-medium text-foreground">对话</Text>
          <SmeltButton
            label={terminal() ? "关终端" : "开终端"}
            onPress={() => terminal.set(terminal() ? "" : Smelt.newSessionId())}
          />
        </View>
        <Scroll style={{ flex: 1 }}>
          <View class="gap-0.5 px-2 pb-2">
            {conversations().map((conversation) => (
              <View
                role="button"
                onPress={() => picked.set(conversation.id)}
                class={`rounded-(--radius) px-2 py-1.5 active:bg-accent ${
                  picked() === conversation.id ? "bg-accent" : ""
                }`}
              >
                <Text class="text-sm text-foreground">{conversation.title}</Text>
              </View>
            ))}
          </View>
        </Scroll>
      </View>
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
        {terminal() ? (
          <SmeltView
            spec={{ view: "smelt.terminal/terminal", args: { machine: null, session_id: terminal() } }}
            ends={terminal()}
            class="border-t border-border"
            style={{ flex: 1 }}
          />
        ) : null}
      </View>
    </View>
  );
}

mount(App);
