// 外壳这一包：外壳和跨 VM 插槽占位（slots.tsx）。插件各自跑在独立 QuickJS VM 里，不打在里面。
import { mount } from "nsh";
import { Shell } from "./Shell.tsx";

mount(Shell);
