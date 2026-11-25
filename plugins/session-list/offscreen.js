// 滚到可视区外的、要你看一眼的会话：在列表那一侧的边上浮一个小胶囊（「↓ 2 个需要你」），点一
// 下打开最近的那个；打开了就算看过，再点就轮到下一个。最近的是折起来的一组，打开组里第一段要
// 你看的（打开时会展开它所在的组）。
//
// 要你看的有两种：需要你处理的，和有没看过的提醒的。在跑的、闲着的不算，算上了胶囊会常驻，等
// 于没提示。正选中的那段不算，人正看着它。哪些行要盯由 sidebar.js 在事件里算好（notice）；行在
// 可视区哪边问列表的滚动区。这里只放算法，胶囊画在 list_view.js。

/** 一处有几段要你看的会话：need 需要你处理的，unread 不需要你处理、但有没看过的提醒的。 */
export function empty_tally() {
  return { need: 0, unread: 0 };
}

export function tally_is_empty(tally) {
  return tally.need === 0 && tally.unread === 0;
}

export function add_tally(into, other) {
  into.need += other.need;
  into.unread += other.unread;
}

export function tally_label(tally, t) {
  const parts = [];
  if (tally.need > 0) parts.push(t("offscreen.need", { count: tally.need }));
  if (tally.unread > 0) parts.push(t("offscreen.unread", { count: tally.unread }));
  return parts.join(" · ");
}

/**
 * 按各行在可视区哪边算两边。notices 是 [{ key, tally, session }]，从上到下；above、below 是列表
 * 报来的在可视区上面、下面的行的 key（压在吸顶标题底下的算在上面）。各边记最近的那一行：上面
 * 的取最下面那条，下面的取最上面那条。没画过的行两边都不在，不算。
 * 返回 { above, below }，各是 { tally, nearest: session | null }。
 */
export function hint(notices, above, below) {
  const up = new Set(above);
  const down = new Set(below);
  const side = () => ({ tally: empty_tally(), nearest: null });
  const result = { above: side(), below: side() };
  for (const notice of notices) {
    if (up.has(notice.key)) {
      add_tally(result.above.tally, notice.tally);
      result.above.nearest = notice.session;
    } else if (down.has(notice.key)) {
      add_tally(result.below.tally, notice.tally);
      result.below.nearest ??= notice.session;
    }
  }
  return result;
}
