// 测量用的会话列表：80 行，每行图标、两行字、时间，和侧栏差不多大。`spin` 时第一行的图标
// 换成转圈（NSH 原生动画，每帧不经 JS）。

import { Scroll, Text, View } from "nsh";

const ROWS = Array.from({ length: 80 }, (_, i) => ({
  title: `会话 ${i}：修一下侧栏在毛玻璃和背景图之间闪的问题`,
  place: `~/wawa/smelt · js-plugins · ${i % 7} 个改动`,
  time: `${(i * 7) % 60} 分钟前`,
}));

export function Rows(props: { spin: boolean }) {
  return (
    <Scroll style={{ flex: 1 }}>
      <View class="gap-0.5 p-2">
        {ROWS.map((row, i) => (
          <View class="flex-row items-center gap-2 rounded-md px-2 py-1.5">
            {props.spin && i === 0 ? (
              <View class="size-3 rounded-full border-2 border-foreground border-t-transparent animate-spin" />
            ) : (
              <View class="size-3 rounded-sm bg-muted" />
            )}
            <View class="flex-1 gap-0.5 overflow-hidden">
              <Text class="text-sm text-foreground">{row.title}</Text>
              <Text class="text-xs text-muted-foreground">{row.place}</Text>
            </View>
            <Text class="text-xs text-muted-foreground">{row.time}</Text>
          </View>
        ))}
      </View>
    </Scroll>
  );
}
