// 外壳和插件之间只隔这一张表：换一个插件只换它那几格，别的格子和外壳里的组件原样留着。
import { ErrorBoundary, signal, Text, View } from "nsh";

type Component = (props: { args?: unknown }) => unknown;
type Views = Record<string, unknown>;
type State = { views?: Views; failed?: boolean };

function cell() {
  return signal<State>({});
}

const cells = new Map<string, ReturnType<typeof cell>>();

export function cellOf(id: string) {
  if (!cells.has(id)) cells.set(id, cell());
  return cells.get(id)!;
}

function Failed(props: { id: string }) {
  return <Text class="text-xs text-destructive">{props.id} 出错了</Text>;
}

/** 插件 `id` 入口文件导出的一个视图；不写 `view` 就是默认导出。 */
export function Slot(props: { id: string; view?: string; args?: unknown }) {
  const held = cellOf(props.id);
  const content = () => {
    const { views, failed } = held();
    if (failed) return <Failed id={props.id} />;
    const Plugin = views?.[props.view ?? "default"] as Component | undefined;
    return Plugin ? (
      <ErrorBoundary fallback={<Failed id={props.id} />}>
        <Plugin args={props.args} />
      </ErrorBoundary>
    ) : null;
  };
  return <View>{content()}</View>;
}
