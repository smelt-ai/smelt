// 插件里几个文件共用的小工具。不 import 宿主模块，逻辑层和视图层都能用。

/** 失败给人看的那句话：Error 取 message，别的转成字。 */
export function message(error) {
  return error?.message ?? String(error);
}

/** Smelt 自带的线条图标的资源路径。 */
export const icon = (name) => `icons/${name}.svg`;

/** 套上 theme().type 里的一种字号：字号、行高、字重一起给。 */
export function typed(el, style) {
  return el.text_size(style.text_size).line_height(style.line_height).font_weight(style.font_weight);
}
