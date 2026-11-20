// 从运行时的工作区目录里挑出归档的对话，按机器、项目分好组。纯函数：不 import 宿主模块，路径规则和文案
// 由 main.js 拼好传进来，测试换成假的。
//
// path：{ clean, parent, project_root_of, labels }（同 smelt-plugin-api 的 path_clean、path_parent、
// project_root_of、project_labels），t 同 smelt-plugin-api 的 t。

/**
 * 一台机器的归档分组；没有归档是 null。now 是当前的 unix 秒，用来决定时间写不写年份。
 *
 * 结果：{ machine, name, unavailable, projects }，projects 最近归档的项目在前，每个是
 * { root, label, rows }（root 是项目根目录，认不出项目时是对话的工作目录，只当分组的身份和悬停提示用），
 * rows 最近归档的在前，每行见 archived_row。
 */
export function machine_group(path, t, { machine, name, unavailable }, style, directory, now) {
  const archived = directory.conversations
    .filter((conversation) => conversation.archived_at != null)
    .sort((a, b) => b.archived_at - a.archived_at);
  if (archived.length === 0) return null;

  // 按出现先后（也就是最近归档的项目在前）收进各组。
  const groups = [];
  for (const conversation of archived) {
    const root = project_of(path, style, directory, conversation);
    const row = archived_row(path, t, style, directory, conversation, root, now);
    const clean = path.clean(style, root);
    const group = groups.find((group) => path.clean(style, group.root) === clean);
    if (group) group.rows.push(row);
    else groups.push({ root, rows: [row] });
  }
  const labels = path.labels(
    style,
    groups.map((group) => group.root),
  );
  return {
    machine,
    name,
    unavailable,
    projects: groups.map((group, index) => ({ root: group.root, label: labels[index], rows: group.rows })),
  };
}

/**
 * 对话归哪个项目：绑定的工作树所属的项目；原工作树已关的按它的路径推（工作树开在
 * `<项目>/worktrees/<任务>`）；否则按工作目录落在哪个打开的项目里；都认不出就按工作目录自成一组。
 */
function project_of(path, style, directory, conversation) {
  const workset =
    conversation.workset_id == null ? null : directory.worksets.find((workset) => workset.id === conversation.workset_id);
  if (workset) return workset.project_root;
  if (conversation.closed_workset_root != null) {
    const worktrees = path.parent(style, conversation.closed_workset_root);
    const root = worktrees == null ? null : path.parent(style, worktrees);
    if (root != null) return root;
  }
  const cwd = cwd_of(conversation);
  return path.project_root_of(style, directory.projects, cwd) ?? cwd;
}

function cwd_of(conversation) {
  return conversation.acp?.cwd ?? conversation.terminals[0]?.cwd ?? "";
}

/**
 * 一段归档的对话：id 是工作区目录里的对话 id（恢复、删除都按它）；is_conversation 是 ACP 对话，否则是一组
 * 终端；when 是「归档于 …」，空的是不知道时间；session_id 是恢复以后跳过去用的守护会话 id（对话的 sid、
 * 终端组的第一格，终端还没起过的没有）；closed_workset 是原工作树已经关掉，恢复前要另选一个：
 * { old_root, candidates: [{ id, label }] }，candidates 是同一项目下还在的工作树。
 */
function archived_row(path, t, style, directory, conversation, project_root, now) {
  const closed_workset =
    conversation.closed_workset_root != null && conversation.workset_id == null
      ? {
          old_root: conversation.closed_workset_root,
          candidates: directory.worksets
            .filter((workset) => path.clean(style, workset.project_root) === path.clean(style, project_root))
            .map((workset) => ({ id: workset.id, label: `${workset.task} · ${workset.branch}` })),
        }
      : null;
  return {
    id: conversation.id,
    title: title_of(t, conversation),
    is_conversation: conversation.acp != null,
    when: archived_when(t, conversation.archived_at, now),
    session_id: conversation.acp != null ? conversation.acp.sid : (conversation.terminals[0]?.id ?? null),
    closed_workset,
  };
}

/** 对话的名字：用户改的、agent 起的、终端的，都没有就按种类叫。 */
export function title_of(t, conversation) {
  const first = conversation.terminals[0];
  const title =
    conversation.custom_title ??
    conversation.acp?.session_title ??
    (first ? (first.custom_title ?? first.launch_label) : null);
  if (title != null && title.trim() !== "") return title;
  return t(conversation.acp != null ? "row.untitled" : "row.terminal");
}

const pad = (n) => String(n).padStart(2, "0");

/** 「归档于 11-14 10:32」，不是今年的写年份。按本地时区。 */
export function archived_when(t, at, now) {
  if (at == null) return "";
  const when = new Date(at * 1000);
  const today = new Date(now * 1000);
  const date = `${pad(when.getMonth() + 1)}-${pad(when.getDate())}`;
  const time =
    when.getFullYear() === today.getFullYear()
      ? `${date} ${pad(when.getHours())}:${pad(when.getMinutes())}`
      : `${when.getFullYear()}-${date}`;
  return t("row.archived_at", { time });
}

/** 正在恢复、删除的那一段的身份。 */
export function busy_key(machine, id) {
  return JSON.stringify([machine, id]);
}

/** 两份分组一样：只由字、数、null 组成，按字面比。 */
export function same_state(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}
