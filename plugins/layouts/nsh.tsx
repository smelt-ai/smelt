// 随程序带的工作台首页与出厂默认布局（原 crates/plugins/layouts）。

import { bootTask, initHost, registerLayouts, registerPages, text } from "smelt/sdk";
import { STANDARD_NAME, WORKBENCH, WORKBENCH_INDEX, WORKBENCH_NAME } from "./contract.js";
import standard from "./layouts/standard.json";

bootTask(async () => {
  await initHost();
  await registerPages([
    {
      name: WORKBENCH_NAME,
      title: text("page.workbench"),
      home: true,
      index: WORKBENCH_INDEX,
    },
  ]);
  await registerLayouts([
    {
      page: WORKBENCH,
      name: STANDARD_NAME,
      title: text("layout.standard"),
      state: standard,
      // 左边的导航透侧栏材质；右边的文件那组是常看的内容，和中间一样铺实色。
      materials: { left: "sidebar" },
    },
  ]);
});
