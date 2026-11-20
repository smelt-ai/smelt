// 测试用的 t() 和路径函数：宿主在 App 里给的那几样，在 Node 里照同样的规则做个最小的。
import { readFileSync } from "node:fs";

/** 读 locales/ui.yml 里一种语言的文案：键是点连起来的路径。 */
export function load_catalog(locale = "en") {
  const text = readFileSync(new URL("../locales/ui.yml", import.meta.url), "utf8");
  const catalog = new Map();
  const path = [];
  for (const line of text.split("\n")) {
    const match = /^( *)([A-Za-z0-9_-]+):\s*(.*)$/.exec(line);
    if (!match) continue;
    const depth = match[1].length / 2;
    path.length = depth;
    const [, , key, rest] = match;
    if (rest === "") {
      path.push(key);
    } else if (key === locale) {
      catalog.set(path.join("."), JSON.parse(rest));
    }
  }
  return catalog;
}

const catalog = load_catalog();

/** 同宿主的 t()：%{名字} 换成参数，找不到的键原样返回。 */
export function t(key, params = {}) {
  const text = catalog.get(key);
  if (text === undefined) return key;
  return text.replace(/%\{(\w+)\}/g, (all, name) => (name in params ? String(params[name]) : all));
}

/** POSIX 的路径规则（同 PathStyle::Posix）。 */
export const path = {
  clean(_style, value) {
    const absolute = value.startsWith("/");
    const parts = [];
    for (const part of value.split("/")) {
      if (part === "" || part === ".") continue;
      if (part === "..") {
        if (parts.length && parts[parts.length - 1] !== "..") parts.pop();
        else if (!absolute) parts.push(part);
      } else parts.push(part);
    }
    const joined = parts.join("/");
    return absolute ? `/${joined}` : joined;
  },
  file_name(_style, value) {
    const parts = value.split("/").filter(Boolean);
    return parts.length ? parts[parts.length - 1] : null;
  },
  is_absolute(_style, value) {
    return value.startsWith("/");
  },
  starts_with(_style, value, base) {
    return value === base || value.startsWith(base.endsWith("/") ? base : `${base}/`);
  },
  join(_style, base, child) {
    if (child.startsWith("/")) return child;
    return base.endsWith("/") ? `${base}${child}` : `${base}/${child}`;
  },
};
