import { describe, expect, test } from "bun:test";
import { FileContentCache } from "../cache.js";

const text = (body, version) => ({ kind: "text", text: body, version });

describe("FileContentCache", () => {
  test("超上限淘汰最久没用的，get 算用过", () => {
    const cache = new FileContentCache(10);
    cache.put(null, "/a", text("1234", "v1"));
    cache.put(null, "/b", text("1234", "v2"));
    expect(cache.totalBytes).toBe(8);

    expect(cache.get(null, "/a")).not.toBeNull();
    cache.put(null, "/c", text("1234", "v3"));
    expect(cache.get(null, "/a")).not.toBeNull();
    expect(cache.get(null, "/b")).toBeNull();
    expect(cache.get(null, "/c")).not.toBeNull();
    expect(cache.totalBytes).toBe(8);
  });

  test("覆写按新大小记账，超过一半上限的不缓存并清掉旧的", () => {
    const cache = new FileContentCache(10);
    cache.put(null, "/a", text("12345", "v1"));
    expect(cache.totalBytes).toBe(5);
    cache.put(null, "/a", text("12", "v2"));
    expect(cache.totalBytes).toBe(2);
    cache.put(null, "/a", text("123456", "v3"));
    expect(cache.get(null, "/a")).toBeNull();
    expect(cache.totalBytes).toBe(0);
  });

  test("removeUnder 只删那台机器上那棵子树", () => {
    const cache = new FileContentCache(1024);
    // 简化的 Windows 前缀规则：相等或后面紧跟分隔符。
    const startsWith = (path, base) =>
      path.toLowerCase() === base.toLowerCase() ||
      path.toLowerCase().startsWith(`${base.toLowerCase()}\\`);
    cache.put("m1", "E:\\proj\\a.md", text("a", "1"));
    cache.put("m1", "E:\\proj\\sub\\b.md", text("b", "2"));
    cache.put("m1", "E:\\project\\c.md", text("c", "3"));
    cache.put("m2", "E:\\proj\\a.md", text("d", "4"));

    cache.removeUnder("m1", "E:\\proj", startsWith);

    expect(cache.get("m1", "E:\\proj\\a.md")).toBeNull();
    expect(cache.get("m1", "E:\\proj\\sub\\b.md")).toBeNull();
    expect(cache.get("m1", "E:\\project\\c.md")).not.toBeNull();
    expect(cache.get("m2", "E:\\proj\\a.md")).not.toBeNull();
    expect(cache.totalBytes).toBe(2);
  });
});
