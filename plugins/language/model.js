// 语言选择纯逻辑：沿用旧 SQLite 库（locale 为 NULL 表示跟随系统），不依赖 NSH/UI。

export const SYSTEM_VALUE = "__system__";

export const MIGRATIONS = [
  `CREATE TABLE IF NOT EXISTS settings (
      id INTEGER PRIMARY KEY CHECK (id = 1),
      locale TEXT
  );`,
];

/** 把库里存的语言代码归到当前版本支持的集合里；未知或为空按跟随系统（null）。 */
export function normalizeChoice(saved, locales) {
  if (typeof saved !== "string" || saved.length === 0) return null;
  const hit = locales.find((item) => item.code === saved);
  return hit ? hit.code : null;
}

/** 语言代码在它自己语言里的叫法（不翻译，换错语言也能找回来）。 */
export function nativeName(code, locales) {
  return locales.find((item) => item.code === code)?.name ?? "";
}

/** 构造下拉选项：「跟随系统（<系统语言>）」排最前，后面是各受支持语言。 */
export function buildOptions(locales, systemLocale, t) {
  const systemLabel = t("option.system", { language: nativeName(systemLocale, locales) });
  return [
    { value: SYSTEM_VALUE, title: systemLabel },
    ...locales.map((item) => ({ value: item.code, title: item.name })),
  ];
}

export function choiceToSelectValue(choice) {
  return choice ?? SYSTEM_VALUE;
}

export function selectValueToChoice(value, locales) {
  if (!value || value === SYSTEM_VALUE) return null;
  return normalizeChoice(value, locales);
}
