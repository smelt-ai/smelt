// 试点：NSH 写的一块面板，嵌在 smelt 的停靠区里（`smelt-nsh`）。
// 量四件事：signal 局部更新；宿主的 Rust 组件（smelt-ui 按钮）放进来；宿主推数据过来
// （登记的视图，`Smelt.on`）；从 JS 调回宿主主线程（`Smelt.openView`）。

import { mount, nativeComponent, nativeModule, Scroll, signal, Text, View } from "nsh";

interface ViewEntry { id: string; title: string }

/** `smelt/pilot` 的 `Smelt`（`smelt-nsh` 的 `src/module.rs`）。 */
interface SmeltApi {
  /** 现在登记的视图，之后每变一次再给一次；返回停止监听的函数。 */
  on(event: "views", listener: (views: ViewEntry[]) => void): () => void;
  /** 在宿主里打开这个视图；打不开是 false。 */
  openView(id: string): Promise<boolean>;
}

const { Smelt } = nativeModule<{ Smelt: SmeltApi }>("smelt/pilot");

/** smelt-ui 的按钮，宿主登记的 GPUI 组件。 */
const SmeltButton = nativeComponent<{ label: string }, { press: {} }>("smelt.button", {
  props: ["label"],
  events: ["press"],
});

const views = signal<ViewEntry[]>([]);
Smelt.on("views", (list) => views.set(list));

function App() {
  const count = signal(0);
  const failed = signal("");

  async function open(id: string) {
    failed.set((await Smelt.openView(id)) ? "" : id);
  }

  return (
    <Scroll style={{ flex: 1 }}>
      <View class="gap-4 p-4">
        <View class="flex-row items-center gap-3">
          <View
            role="button"
            onPress={() => count.set(count() + 1)}
            class="rounded-(--radius) bg-primary px-3 py-1.5 active:opacity-80"
          >
            <Text class="text-sm font-medium text-primary-foreground">{`NSH 按钮 ${count()}`}</Text>
          </View>
          <SmeltButton label="smelt-ui 按钮 +1" onPress={() => count.set(count() + 1)} />
        </View>
        <View class="gap-0.5">
          {views().map((view) => (
            <View
              role="button"
              onPress={() => open(view.id)}
              class="flex-row items-center justify-between rounded-(--radius) px-2 py-1.5 active:bg-accent"
            >
              <Text class="text-sm text-foreground">{view.title}</Text>
              <Text class="text-xs text-muted-foreground">{view.id}</Text>
            </View>
          ))}
        </View>
        {failed() ? <Text class="text-sm text-destructive">{`打不开 ${failed()}`}</Text> : null}
      </View>
    </Scroll>
  );
}

mount(App);
