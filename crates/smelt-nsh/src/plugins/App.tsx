// 外壳这一包：外壳、插槽表、装插件的运行时。插件不打在里面，各自一段脚本，之后送进来。
import { mount } from "nsh";
import "./runtime.ts";
import { Shell } from "./Shell.tsx";

mount(Shell);
