// Guest 插件运行时：把插件导出的视图接到外壳的挂载通道。
// 每个实例持有一份响应式 args signal，收到 update 时原地更新 props.args，不重建组件子树；
// 组件体内抛错由 ErrorBoundary 兜底，不影响同插件其它实例或宿主。
import { ErrorBoundary, host, mountSlot, signal, Text } from "nsh";

type Component = (props: { args?: unknown }) => unknown;
type Views = Record<string, unknown>;

interface SyncOp {
  op: "mount" | "update" | "unmount";
  instance: string;
  target?: string;
  view?: string;
  args?: unknown;
}

interface Mounted {
  setArgs: (value: unknown) => void;
  dispose: () => void;
}

const pendingBootTasks: Promise<unknown>[] = [];
const BOOT_TIMEOUT_MS = 3_000;

/** 登记插件启动期异步初始化任务（如注册页/布局/读库恢复设置）；全部完成后才上报 smelt.plugin.ready。 */
export function bootTask(work: (() => Promise<unknown>) | Promise<unknown>): Promise<unknown> {
  const task = Promise.resolve()
    .then(() => (typeof work === "function" ? work() : work))
    .catch((error) => {
      console.error("[smelt/provider] bootTask failed:", error);
    });
  pendingBootTasks.push(task);
  return task;
}

async function flushBootTasks(): Promise<void> {
  while (pendingBootTasks.length > 0) {
    await Promise.allSettled(pendingBootTasks.splice(0));
  }
}

function Failed(props: { id: string }) {
  return <Text class="text-xs text-destructive">{props.id} 出错了</Text>;
}

/** 将插件导出的视图注册给引擎内宿主挂载路由器，并按需 mountSlot 到外壳分配的容器节点。 */
export function provideViews(pluginId: string, views: Views) {
  const mounts = new Map<string, Mounted>();

  function unmount(instance: string) {
    const current = mounts.get(instance);
    if (!current) return;
    mounts.delete(instance);
    current.dispose();
  }

  function mountOne(
    instance: string,
    target: string,
    viewName: string,
    initialArgs: unknown,
  ) {
    unmount(instance);
    const Comp = views[viewName] as Component | undefined;
    if (typeof Comp !== "function") return;
    const args = signal<unknown>(initialArgs);
    const dispose = mountSlot(target, () => (
      <ErrorBoundary
        fallback={(error: unknown) => {
          console.error(`[plugin:${pluginId}] render error:`, error);
          return <Failed id={pluginId} />;
        }}
      >
        <Comp args={args()} />
      </ErrorBoundary>
    ));
    mounts.set(instance, {
      setArgs: (value) => args.set(value),
      dispose,
    });
  }

  host.on("smelt.slot.sync", (data: unknown) => {
    if (!Array.isArray(data)) return;
    for (const item of data as SyncOp[]) {
      if (item.op === "mount" && item.target && item.view) {
        mountOne(item.instance, item.target, item.view, item.args);
      } else if (item.op === "update") {
        const entry = mounts.get(item.instance);
        if (entry) {
          entry.setArgs(item.args);
        }
      } else if (item.op === "unmount") {
        unmount(item.instance);
      }
    }
  });

  const exported = Object.keys(views).filter((key) => typeof views[key] === "function");
  const announceReady = () => host.post("smelt.plugin.ready", { views: exported });
  if (pendingBootTasks.length === 0) {
    announceReady();
  } else {
    let timer: ReturnType<typeof setTimeout> | undefined;
    void Promise.race([
      flushBootTasks(),
      new Promise<void>((resolve) => {
        timer = setTimeout(resolve, BOOT_TIMEOUT_MS);
      }),
    ]).finally(() => {
      if (timer !== undefined) clearTimeout(timer);
      announceReady();
    });
  }
}
