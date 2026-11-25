// 方案 2，重一点：右边四块终端（2×2），都是嵌进来的原生组件。

import { mount, nativeComponent, View } from "nsh";
import { Rows } from "./rows";

const Terminal = nativeComponent<{}, {}>("bench.terminal", { props: [], events: [] });

mount(() => (
  <View class="flex-1 flex-row">
    <View style={{ width: 280 }}>
      <Rows spin={true} />
    </View>
    <View style={{ flex: 1 }}>
      <View class="flex-1 flex-row">
        <Terminal style={{ flex: 1 }} />
        <Terminal style={{ flex: 1 }} />
      </View>
      <View class="flex-1 flex-row">
        <Terminal style={{ flex: 1 }} />
        <Terminal style={{ flex: 1 }} />
      </View>
    </View>
  </View>
));
