import { expect, test } from "bun:test";

import { bucket_label, bucket_of_time, buckets } from "../group.js";
import { t } from "./helpers.mjs";

const flatten = (groups) => groups.flatMap(([, ixs]) => ixs);
const labels = (groups) => groups.map(([bucket]) => bucket_label(bucket, t));
const now = () => new Date(2026, 10, 17, 18, 0, 0);
/** now 那天往前 days 天的中午（unix 秒）。 */
const noon_days_ago = (days) => new Date(2026, 10, 17 - days, 12, 0, 0).getTime() / 1000;

test("按状态的组按画出来的顺序", () => {
  const rows = [
    ["idle", 0],
    ["running", 0],
    ["needs_you", 0],
    ["idle", 0],
  ];
  const groups = buckets("status", rows, now());
  expect(labels(groups)).toStrictEqual(["Needs you", "Running", "Idle"]);
  expect(flatten(groups)).toStrictEqual([2, 1, 0, 3]);
});

test("按状态分时空组不出", () => {
  const groups = buckets(
    "status",
    [
      ["idle", 5],
      ["idle", 9],
    ],
    now(),
  );
  expect(groups).toStrictEqual([["Idle", [0, 1]]]);
  expect(buckets("status", [], now())).toStrictEqual([]);
});

test("按项目不分组", () => {
  expect(
    buckets(
      "project",
      [
        ["needs_you", 1],
        ["idle", 2],
      ],
      now(),
    ),
  ).toStrictEqual([]);
});

test("按时间按日历分组、组里从新到旧", () => {
  const rows = [0, 2, 1, 8].map((days) => ["idle", noon_days_ago(days)]);
  const groups = buckets("last_updated", rows, now());
  expect(labels(groups)).toStrictEqual(["Today", "Yesterday", "Last 7 days", "Earlier"]);
  expect(flatten(groups)).toStrictEqual([0, 2, 1, 3]);
});

test("一个组里从新到旧，一样新的保留原来的顺序", () => {
  const today = noon_days_ago(0);
  const rows = [
    ["idle", today - 60],
    ["idle", today],
    ["idle", today - 60],
  ];
  expect(buckets("last_updated", rows, now())).toStrictEqual([["Today", [1, 0, 2]]]);
});

test("时钟快了算今天，不知道的算更早", () => {
  const future = now().getTime() / 1000 + 3 * 86_400;
  expect(bucket_of_time(future, now())).toBe("Today");
  expect(bucket_of_time(0, now())).toBe("Earlier");
  expect(bucket_of_time(Number.MAX_SAFE_INTEGER, now())).toBe("Earlier");
});
