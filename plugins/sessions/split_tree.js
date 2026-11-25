// 会话分屏二叉树（v2）：纯二元切分（horizontal / vertical），不再套二层子标签。
// 节点形状：
// - { kind: "leaf", id: string }
// - { kind: "split", axis: "horizontal" | "vertical", sizes: [number, number] | null, first: Node, second: Node }

export const LAYOUT_VERSION = 2;

export function leaf(id) {
  return { kind: "leaf", id };
}

export function split(axis, first, second, sizes = null) {
  return {
    kind: "split",
    axis: axis === "vertical" ? "vertical" : "horizontal",
    sizes: validSizes(sizes) ? [sizes[0], sizes[1]] : null,
    first,
    second,
  };
}

function validSizes(sizes) {
  return (
    Array.isArray(sizes) &&
    sizes.length === 2 &&
    typeof sizes[0] === "number" &&
    typeof sizes[1] === "number" &&
    Number.isFinite(sizes[0]) &&
    Number.isFinite(sizes[1]) &&
    sizes[0] > 0 &&
    sizes[1] > 0
  );
}

export function encodeTree(root) {
  return JSON.stringify({ v: LAYOUT_VERSION, root });
}

function parseNode(raw) {
  if (!raw || typeof raw !== "object") return null;
  if (raw.kind === "leaf") {
    return typeof raw.id === "string" && raw.id.length > 0 ? leaf(raw.id) : null;
  }
  if (raw.kind === "split") {
    if (raw.axis !== "horizontal" && raw.axis !== "vertical") return null;
    const first = parseNode(raw.first);
    const second = parseNode(raw.second);
    if (!first || !second) return null;
    return split(raw.axis, first, second, raw.sizes ?? null);
  }
  return null;
}

export function decodeTree(stored) {
  if (typeof stored !== "string" || !stored.trim()) return null;
  try {
    const parsed = JSON.parse(stored);
    if (!parsed || typeof parsed !== "object" || parsed.v !== LAYOUT_VERSION) return null;
    return parseNode(parsed.root);
  } catch {
    return null;
  }
}

export function dedupIds(ids) {
  const seen = new Set();
  const out = [];
  for (const id of ids ?? []) {
    if (typeof id === "string" && id.length > 0 && !seen.has(id)) {
      seen.add(id);
      out.push(id);
    }
  }
  return out;
}

/** 默认摆法：每格一列，左右排开。 */
export function defaultTree(paneIds) {
  const ids = dedupIds(paneIds);
  if (ids.length === 0) return null;
  let tree = leaf(ids[0]);
  for (let i = 1; i < ids.length; i += 1) {
    tree = split("horizontal", tree, leaf(ids[i]), null);
  }
  return tree;
}

function retainNode(node, wanted, placed) {
  if (!node) return null;
  if (node.kind === "leaf") {
    if (wanted.has(node.id) && !placed.has(node.id)) {
      placed.add(node.id);
      return node;
    }
    return null;
  }
  const first = retainNode(node.first, wanted, placed);
  const second = retainNode(node.second, wanted, placed);
  if (first && second) {
    // 两侧都在才保留尺寸；否则坍缩为存活的一侧。
    return split(node.axis, first, second, node.sizes);
  }
  return first ?? second;
}

/** 按存档与目录中的真实格子对账：丢掉不存在的、补齐新增的（挂在最右）。 */
export function restoreTree(storedJson, paneIds) {
  const ids = dedupIds(paneIds);
  if (ids.length === 0) return null;
  const decoded = decodeTree(storedJson);
  if (!decoded) return defaultTree(ids);
  return reconcileTree(decoded, ids);
}

/** 对已有的内存树做目录对账：支持把刚分屏的新格子插到指定格子的右边/下边。 */
export function reconcileTree(currentTree, paneIds, expectedMap = null) {
  const ids = dedupIds(paneIds);
  if (ids.length === 0) return null;
  const wanted = new Set(ids);
  const placed = new Set();
  let tree = retainNode(currentTree, wanted, placed);
  for (const id of ids) {
    if (placed.has(id)) continue;
    placed.add(id);
    const hint = expectedMap?.get(id) ?? null;
    if (hint) {
      tree = splitPaneAt(tree, hint.beside ?? null, hint.axis ?? "horizontal", id);
    } else if (!tree) {
      tree = leaf(id);
    } else {
      tree = split("horizontal", tree, leaf(id), null);
    }
  }
  return tree;
}

/** 在 targetId 那一格旁边切出新格子；targetId 不存在时并到根的右侧。 */
export function splitPaneAt(root, targetId, axis, newPaneId) {
  const dir = axis === "vertical" ? "vertical" : "horizontal";
  if (!root) return leaf(newPaneId);
  if (!targetId) return split(dir, root, leaf(newPaneId), null);
  let replaced = false;
  function visit(node) {
    if (node.kind === "leaf") {
      if (node.id === targetId) {
        replaced = true;
        return split(dir, node, leaf(newPaneId), null);
      }
      return node;
    }
    const first = visit(node.first);
    const second = visit(node.second);
    if (first === node.first && second === node.second) return node;
    return split(node.axis, first, second, node.sizes);
  }
  const next = visit(root);
  return replaced ? next : split(dir, root, leaf(newPaneId), null);
}

/** 移除一格，父分屏自动坍缩为剩余一侧。 */
export function removePane(root, paneId) {
  if (!root) return null;
  if (root.kind === "leaf") return root.id === paneId ? null : root;
  const first = removePane(root.first, paneId);
  const second = removePane(root.second, paneId);
  if (first && second) {
    return first === root.first && second === root.second
      ? root
      : split(root.axis, first, second, root.sizes);
  }
  return first ?? second;
}

/** 按路径更新指定分屏节点的两段尺寸。 */
export function updateSplitSizes(root, path, sizes) {
  if (!root || !validSizes(sizes)) return root;
  function visit(node, depth) {
    if (!node || node.kind !== "split") return node;
    if (depth === path.length) {
      return split(node.axis, node.first, node.second, [sizes[0], sizes[1]]);
    }
    const branch = path[depth];
    if (branch === 0) {
      const nextFirst = visit(node.first, depth + 1);
      return nextFirst === node.first ? node : split(node.axis, nextFirst, node.second, node.sizes);
    }
    const nextSecond = visit(node.second, depth + 1);
    return nextSecond === node.second ? node : split(node.axis, node.first, nextSecond, node.sizes);
  }
  return visit(root, 0);
}

/** 深度优先的格子顺序（轮换焦点按此顺序）。 */
export function leafOrder(root) {
  const out = [];
  function visit(node) {
    if (!node) return;
    if (node.kind === "leaf") {
      out.push(node.id);
      return;
    }
    visit(node.first);
    visit(node.second);
  }
  visit(root);
  return out;
}

/** 轮换焦点到下一格 / 上一格，到头绕回。 */
export function cyclePaneId(order, activeId, forward) {
  const len = order.length;
  if (len === 0) return null;
  const ix = activeId ? order.indexOf(activeId) : -1;
  const cur = ix >= 0 ? ix : 0;
  const next = forward ? (cur + 1) % len : (cur + len - 1) % len;
  return order[next];
}

/** 计算每一格碰到外框的哪几条边，用于分发 safeArea。 */
export function paneTouches(root, outer = { top: true, right: true, bottom: true, left: true }) {
  const map = new Map();
  function visit(node, touches) {
    if (!node) return;
    if (node.kind === "leaf") {
      map.set(node.id, touches);
      return;
    }
    if (node.axis === "horizontal") {
      visit(node.first, { ...touches, right: false });
      visit(node.second, { ...touches, left: false });
    } else {
      visit(node.first, { ...touches, bottom: false });
      visit(node.second, { ...touches, top: false });
    }
  }
  visit(root, outer);
  return map;
}
