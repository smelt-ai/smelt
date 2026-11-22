// 跨 VM 插槽占位：外壳和任意插件都用同一个 `<Slot>`（别名 `SmeltSlot`）摆其它插件的视图。
// 同帧内的多次 mount / update / unmount 经微任务合批，通过 `smelt.slot.ops` 交给引擎内路由器，
// 不回绕 GPUI 主线程；提供方热重载或崩溃时，占位容器不动，由宿主按 instances 表自动重放或标错。
import { cleanup, effect, host, signal, Text, View } from "nsh";

interface SlotOp {
  op: "mount" | "update" | "unmount";
  instance: string;
  target?: string;
  plugin?: string;
  view?: string;
  args?: unknown;
}

const failed = new Map<string, ReturnType<typeof signal<boolean>>>();

function failedOf(plugin: string) {
  let cell = failed.get(plugin);
  if (!cell) {
    cell = signal(false);
    failed.set(plugin, cell);
  }
  return cell;
}

host.on("smelt.slot.failed", (data: any) => {
  if (data && typeof data.plugin === "string") {
    failedOf(data.plugin).set(Boolean(data.failed));
  }
});

// 每个 VM 唯一前缀（由宿主经 host.params 注入含代数的前缀，避免跨 VM 或热重载后实例 id 碰撞）。
const ownerPrefix =
  typeof (host.params as any)?.__smelt_owner === "string"
    ? ((host.params as any).__smelt_owner as string)
    : "shell";

let nextInstance = 0;
let pending: SlotOp[] = [];
let flushing = false;

function enqueue(op: SlotOp) {
  pending.push(op);
  if (!flushing) {
    flushing = true;
    Promise.resolve().then(() => {
      flushing = false;
      const batch = pending;
      pending = [];
      if (batch.length > 0) host.post("smelt.slot.ops", batch);
    });
  }
}

function Failed(props: { id: string }) {
  return <Text class="text-xs text-destructive">{props.id} 出错了</Text>;
}

/** 插件 `id` 入口文件导出的一个视图；不写 `view` 就是默认导出。 */
export function Slot(props: { id: string; view?: string; args?: unknown }) {
  const instance = `${ownerPrefix}:${++nextInstance}`;
  const target = `smelt:i:${instance}`;
  let mounted = false;
  let lastPlugin = "";
  let lastView = "";

  effect(() => {
    const plugin = props.id;
    const view = props.view ?? "default";
    const args = props.args ?? null;
    if (!mounted) {
      mounted = true;
      lastPlugin = plugin;
      lastView = view;
      enqueue({ op: "mount", instance, target, plugin, view, args });
    } else if (plugin !== lastPlugin || view !== lastView) {
      enqueue({ op: "unmount", instance });
      lastPlugin = plugin;
      lastView = view;
      enqueue({ op: "mount", instance, target, plugin, view, args });
    } else {
      enqueue({ op: "update", instance, args });
    }
  });

  cleanup(() => {
    if (mounted) {
      mounted = false;
      enqueue({ op: "unmount", instance });
    }
  });

  return (
    <View slotTarget={target}>
      {failedOf(props.id)() ? <Failed id={props.id} /> : null}
    </View>
  );
}

export const SmeltSlot = Slot;
