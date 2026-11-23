import { describe, expect, test } from "bun:test";

import {
  buildOptions,
  choiceToSelectValue,
  nativeName,
  normalizeChoice,
  selectValueToChoice,
  SYSTEM_VALUE,
} from "../model.js";

const LOCALES = [
  { code: "en", name: "English" },
  { code: "zh-CN", name: "简体中文" },
];

describe("smelt.language model", () => {
  test("已知语言保留，未知或空代码回退为跟随系统", () => {
    expect(normalizeChoice(null, LOCALES)).toBeNull();
    expect(normalizeChoice("", LOCALES)).toBeNull();
    expect(normalizeChoice("zh-CN", LOCALES)).toBe("zh-CN");
    expect(normalizeChoice("tlh", LOCALES)).toBeNull();
  });

  test("下拉选项把跟随系统排在最前且其余显示原生名", () => {
    const t = (key, params) =>
      key === "option.system" ? `System (${params.language})` : key;
    const opts = buildOptions(LOCALES, "zh-CN", t);
    expect(opts).toEqual([
      { value: SYSTEM_VALUE, title: "System (简体中文)" },
      { value: "en", title: "English" },
      { value: "zh-CN", title: "简体中文" },
    ]);
    expect(nativeName("en", LOCALES)).toBe("English");
    expect(choiceToSelectValue(null)).toBe(SYSTEM_VALUE);
    expect(choiceToSelectValue("en")).toBe("en");
    expect(selectValueToChoice(SYSTEM_VALUE, LOCALES)).toBeNull();
    expect(selectValueToChoice("zh-CN", LOCALES)).toBe("zh-CN");
    expect(selectValueToChoice("unknown", LOCALES)).toBeNull();
  });
});
