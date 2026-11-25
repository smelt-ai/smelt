// 外壳专用跨 VM 视图挂载器（仅打进主壳包，不暴露给插件）：
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

// 外壳实例前缀（由宿主经 host.params 注入含代数的前缀，避免引擎重启后实例 id 碰撞）。
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

/** 外壳把插件 `id` 导出的顶层视图挂进当前位置；不写 `view` 就是默认导出。 */
export function Slot(props: {
  id: string;
  view?: string;
  args?: unknown;
  class?: string;
  style?: any;
}) {
  const instance = `${ownerPrefix}:${++nextInstance}`;
  const target = `smelt:i:${instance}`;
  let mounted = false;
  let lastPlugin = "";
  let lastView = "";
  let lastArgsJson: string | undefined;

  effect(() => {
    const plugin = props.id;
    const view = props.view ?? "default";
    const args = props.args ?? null;
    const argsJson = JSON.stringify(args);
    if (!mounted) {
      mounted = true;
      lastPlugin = plugin;
      lastView = view;
      lastArgsJson = argsJson;
      enqueue({ op: "mount", instance, target, plugin, view, args });
    } else if (plugin !== lastPlugin || view !== lastView) {
      enqueue({ op: "unmount", instance });
      lastPlugin = plugin;
      lastView = view;
      lastArgsJson = argsJson;
      enqueue({ op: "mount", instance, target, plugin, view, args });
    } else if (argsJson !== lastArgsJson) {
      lastArgsJson = argsJson;
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
    <View slotTarget={target} class={props.class} style={props.style}>
      <If when={failedOf(props.id)()}>
        <Failed id={props.id} />
      </If>
    </View>
  );
}

export const SmeltSlot = Slot;
