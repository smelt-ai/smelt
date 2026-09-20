// 浏览器插件的前端：只做 chrome（地址栏、书签、前进后退）。
//
// 网页本身由宿主的内容视图渲染——那是一个真正的 WebView，不是 iframe。
// 这个区别是本质的：iframe 是 Web 平台内的嵌套浏览上下文，站点用
// `X-Frame-Options` 就能拒绝（Google、百度都拒绝），而内容视图就是浏览器
// 本身，站点没有理由也没有能力拒绝它。
//
// 两条通道：
//   window.smelt.request(...)  → 宿主转成 Invocation 交给 Shared Bun 插件模块（书签/历史）
//   window.smelt.view.*        → 宿主控制内容视图（导航/位置/前进后退）
//
// 书签和历史全部住在插件模块自己的数据目录，页面不留副本，每次操作都用返回的整份状态
// 重画，避免两边不同步。

const $ = (id) => document.getElementById(id);
const urlInput = $('url');
const star = $('star');

let state = { bookmarks: [], history: [] };
let current = '';
// 前进后退栈由页面自己维护：内容视图的历史在宿主那边，页面读不到。
const stack = [];
let cursor = -1;

async function call(payload) {
  const result = await window.smelt.request(payload);
  if (result && result.state) {
    state = result.state;
    render();
  }
  return result;
}

function hostOf(url) {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function isBookmarked(url) {
  return state.bookmarks.some((bookmark) => bookmark.url === url);
}

function render() {
  // 书签栏
  const bar = $('bookmarks');
  bar.replaceChildren();
  bar.hidden = state.bookmarks.length === 0;
  for (const bookmark of state.bookmarks) {
    const button = document.createElement('button');
    button.textContent = bookmark.title;
    button.title = bookmark.url;
    button.addEventListener('click', () => navigate(bookmark.url));
    bar.append(button);
  }

  renderList($('bookmark-list'), state.bookmarks, '还没有书签', (bookmark) => ({
    url: bookmark.url,
    title: bookmark.title,
    onDelete: () => call({ op: 'bookmark.remove', url: bookmark.url }),
  }));
  renderList($('history-list'), state.history, '还没有记录', (visit) => ({
    url: visit.url,
    title: visit.title,
  }));

  star.classList.toggle('on', isBookmarked(current));
  star.textContent = isBookmarked(current) ? '★' : '☆';
  star.disabled = !current;

  $('back').disabled = cursor <= 0;
  $('forward').disabled = cursor >= stack.length - 1;
}

function renderList(root, items, emptyText, describe) {
  root.replaceChildren();
  if (items.length === 0) {
    const li = document.createElement('li');
    li.className = 'empty';
    li.textContent = emptyText;
    root.append(li);
    return;
  }
  for (const item of items.slice(0, 12)) {
    const { url, title, onDelete } = describe(item);
    const li = document.createElement('li');

    const link = document.createElement('a');
    link.textContent = title;
    link.title = url;
    link.addEventListener('click', () => navigate(url));

    const host = document.createElement('span');
    host.className = 'host';
    host.textContent = hostOf(url);

    li.append(link, host);
    if (onDelete) {
      const del = document.createElement('button');
      del.className = 'del';
      del.textContent = '×';
      del.title = '删除';
      del.addEventListener('click', onDelete);
      li.append(del);
    }
    root.append(li);
  }
}

/// 内容视图应当占据的矩形（相对面板左上角）。宿主按这个位置摆放它。
function viewportRect() {
  const box = document.getElementById('viewport').getBoundingClientRect();
  return {
    x: Math.round(box.left),
    y: Math.round(box.top),
    width: Math.round(box.width),
    height: Math.round(box.height),
  };
}

/// 导航。URL 规范化由插件进程做（它是唯一可信的那一侧），
/// 真正加载网页的是宿主的内容视图。
async function navigate(raw, { record = true } = {}) {
  const result = await call({
    op: 'history.push',
    url: raw,
    title: raw,
    at: Date.now(),
  }).catch((error) => {
    urlInput.setCustomValidity(error.message);
    urlInput.reportValidity();
    setTimeout(() => urlInput.setCustomValidity(''), 2000);
    return null;
  });
  if (!result) return;

  // 插件回的历史第一条就是刚刚规范化过的 URL。
  const url = state.history[0]?.url ?? raw;
  current = url;
  urlInput.value = url;

  if (record) {
    stack.splice(cursor + 1);
    stack.push(url);
    cursor = stack.length - 1;
  }

  $('start').hidden = true;
  window.smelt.view.navigate(url, viewportRect());
  render();
}

// 面板尺寸变了要让宿主跟着挪内容视图——它是独立于页面布局的原生视图，
// 不会随 DOM 自动重排。
const resize = new ResizeObserver(() => {
  if (current) window.smelt.view.setBounds(viewportRect());
});
resize.observe(document.getElementById('viewport'));

// 宿主推回来的导航状态：地址栏和标题跟着真实加载结果走，而不是我们请求的值
// （重定向、错误页都会让两者不同）。
window.smelt.on((message) => {
  if (!message || message.kind !== 'view.state') return;
  const view = message.state || {};
  if (view.url && view.url !== 'about:blank') {
    current = view.url;
    if (document.activeElement !== urlInput) urlInput.value = view.url;
  }
  render();
});

$('omnibox').addEventListener('submit', (event) => {
  event.preventDefault();
  const value = urlInput.value.trim();
  if (value) navigate(value);
});

$('back').addEventListener('click', () => {
  if (cursor > 0) navigate(stack[--cursor], { record: false });
});
$('forward').addEventListener('click', () => {
  if (cursor < stack.length - 1) navigate(stack[++cursor], { record: false });
});
$('reload').addEventListener('click', () => {
  if (current) window.smelt.view.reload();
});

star.addEventListener('click', () => {
  if (!current) return;
  const op = isBookmarked(current) ? 'bookmark.remove' : 'bookmark.add';
  call({ op, url: current, title: hostOf(current) });
});

$('clear-history').addEventListener('click', () => call({ op: 'history.clear' }));

call({ op: 'state' }).then((result) => {
  $('meta').textContent = `插件版本 ${result.version} · 面板 ${window.smelt.panelId}`;
});
