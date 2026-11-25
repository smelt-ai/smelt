// 方案 2：整窗一块 NSH，左边会话列表，右边终端是嵌进来的原生组件（`bench.terminal`）。

import { mount, nativeComponent, View } from "nsh";
import { Rows } from "./rows";

const Terminal = nativeComponent<{}, {}>("bench.terminal", { props: [], events: [] });

mount(() => (
  <View class="flex-1 flex-row">
    <View style={{ width: 280 }}>
      <Rows spin={true} />
    </View>
    <Terminal style={{ flex: 1 }} />
  </View>
));
