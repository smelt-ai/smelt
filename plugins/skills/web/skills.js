// 技能面板的前端。
//
// 两条通道，各自职责清楚：
//   window.smelt.request({op,...}) → 宿主转成 Invocation 交给插件进程（扫盘与 CRUD）
//   window.smelt.host(command,...) → 宿主代办页面做不到的事（原生目录框、访达）
//
// 页面不留业务副本：每次操作都用插件回的整份状态重画，磁盘才是唯一真相。
// 当前项目也不由页面猜——它是宿主的 UI 上下文，靠 `ui.context` 拉一次、
// 之后由宿主推变化。
//
// 界面上只有两个概念：**作用范围**（通用 / 指定 agent）和**层级**（用户级 / 项目级）。
// 兼容软链是实现细节，只在坏掉时才浮出来。

const $ = (id) => document.getElementById(id);
const overlay = $('overlay');
const dialog = $('dialog');

let state = { skills: [], agents: [], project_root: null, legacy_count: 0, broken_links: [] };
let projectRoot = null;
let booted = false;

function el(tag, props, ...children) {
  const node = Object.assign(document.createElement(tag), props || {});
  for (const child of children.flat()) {
    if (child != null && child !== false) node.append(child);
  }
  return node;
}

async function call(op, params) {
  const reply = await window.smelt.request({ op, project_root: projectRoot, ...(params || {}) });
  return reply && reply.data;
}

/// 跑一次 op；插件回的是整份状态就重画。失败只报给用户，不改本地状态——
/// 磁盘没变，界面也不该变。
async function run(op, params) {
  try {
    const data = await call(op, params);
    if (data && Array.isArray(data.skills)) {
      state = data;
      render();
    }
    return data;
  } catch (error) {
    notify(error && error.message ? error.message : String(error));
    return null;
  }
}

function load() {
  return run('state');
}

// ── 上下文 ────────────────────────────────────────────────────────────────

function applyContext(context) {
  const next = (context && context.project_root) || null;
  const changed = next !== projectRoot;
  projectRoot = next;
  if (changed && booted) load();
}

// ── 渲染 ──────────────────────────────────────────────────────────────────

function render() {
  $('count').textContent = state.skills.length ? String(state.skills.length) : '';
  const list = $('list');
  list.replaceChildren();

  if (state.legacy_count) list.append(legacyBanner());
  if (state.broken_links && state.broken_links.length) list.append(brokenBanner());

  if (!state.skills.length) {
    list.append(
      el('p', { className: 'empty' }, '还没有 skill', el('code', {}, `~/${state.universal_dir || '.agents/skills'}/<名字>/SKILL.md`)),
    );
    return;
  }

  let lastScope = null;
  for (const skill of state.skills) {
    if (skill.project_scope !== lastScope) {
      lastScope = skill.project_scope;
      list.append(el('div', { className: 'group' }, skill.project_scope ? '项目级' : '用户级'));
    }
    list.append(row(skill));
  }
}

/// 悬空的兼容链接不属于任何 skill，面板里够不着，只能单独给个清理入口——
/// 否则它们会一直躺在 agent 目录里，让 agent 每次都去读一个不存在的目标。
function brokenBanner() {
  const count = state.broken_links.length;
  return el(
    'div',
    { className: 'banner' },
    el('span', {}, `${count} 条兼容链接指向已经不存在的目标`),
    el('button', {
      className: 'link accent',
      textContent: '查看并清理',
      onclick: openBrokenDialog,
    }),
  );
}

function openBrokenDialog() {
  openDialog(
    '清理失效链接',
    [
      el('p', {}, `以下 ${state.broken_links.length} 条链接指向的目标已经不存在：`),
      el(
        'ul',
        { className: 'preview' },
        state.broken_links.map((link) => el('li', { textContent: link.display_path })),
      ),
      el('p', { className: 'hint' }, '只删除这些链接本身，不会动任何真实的 skill 目录。'),
    ],
    [
      cancelButton(),
      button('清理', 'destructive', async () => {
        closeDialog();
        const data = await run('links.prune');
        if (data) announce('清理完成', [`已移除 ${(data.pruned || []).length} 条失效链接。`]);
      }),
    ],
  );
}

/// 旧的 .smelt/skills 私有目录没有任何 agent 会读，横幅只提示一次性搬家，
/// 不搬也不会丢东西——旧位置照样列在下面。
function legacyBanner() {
  return el(
    'div',
    { className: 'banner' },
    el('span', {}, `${state.legacy_count} 个 skill 还放在 smelt 私有目录，没有 agent 会读到`),
    el('button', {
      className: 'link accent',
      textContent: `迁移到 ${state.universal_dir || '.agents/skills'}`,
      onclick: migrateLegacy,
    }),
  );
}

async function migrateLegacy() {
  const data = await run('legacy.migrate');
  // 出错时 run 已经弹过窗了，这里不重复。
  if (!data) return;
  const migrated = data.migrated || [];
  const skipped = data.skipped || [];
  const target = state.universal_dir || '.agents/skills';
  // 一次操作必须有一句交代。「点了没反应」比报错更让人不知所措。
  const lines = [];
  if (migrated.length) lines.push(`已迁移 ${migrated.length} 个到 ${target}：${migrated.join('、')}`);
  if (skipped.length) {
    lines.push(
      `${skipped.length} 个迁不动：${target} 里已有同名的 skill。` +
        `请在列表里点它们的「N 份同名副本」逐个选定要留哪份，再迁一次：${skipped.join('、')}`,
    );
  }
  if (!lines.length) lines.push('没有需要迁移的 skill。');
  announce(migrated.length && !skipped.length ? '迁移完成' : '迁移结果', lines);
}

/// 作用范围一枚 chip 说清：通用 / 具体 agent / 旧位置，坏了就变成警告。
function scopeChip(skill) {
  if (skill.conflicts.length) {
    return el('span', { className: 'chip bad', textContent: `${skill.conflicts.length + 1} 份同名副本` });
  }
  if (skill.placement === 'legacy') {
    return el('span', { className: 'chip warn', textContent: '旧位置', title: skill.display_dir });
  }
  if (skill.placement === 'universal') {
    return skill.missing_bridges.length
      ? el('span', {
          className: 'chip warn',
          textContent: `通用（缺 ${skill.missing_bridges.length} 条兼容链接）`,
          title: `${skill.missing_bridges.join('、')} 看不到这个 skill`,
        })
      : el('span', { className: 'chip', textContent: '通用', title: skill.display_dir });
  }
  return el('span', {
    className: 'chip',
    textContent: skill.agents.join(' · ') || skill.agent || '未生效',
    title: skill.display_dir,
  });
}

function row(skill) {
  const actions = el('div', { className: 'actions' });
  const action = (label, className, onClick) =>
    actions.append(el('button', { className: `link ${className || ''}`, textContent: label, onclick: onClick }));

  if (skill.conflicts.length) {
    action('处理冲突', 'warn', () => openConflictDialog(skill));
  } else {
    action('作用范围', '', () => openScopeDialog(skill));
    if (skill.missing_bridges.length && skill.placement !== 'legacy') {
      action('修复链接', 'warn', () => run('bridges.repair', { dir: skill.dir }));
    }
  }
  action('编辑', '', () => openEditDialog(skill));
  action('访达', '', () => reveal(skill.dir));
  action('删除', 'danger', () => openDeleteDialog(skill));

  return el(
    'div',
    { className: 'row' },
    el(
      'div',
      { className: 'head' },
      el('span', { className: `dot ${skill.project_scope ? 'project' : 'user'}` }),
      el('span', { className: 'name', textContent: skill.name, title: skill.display_dir }),
      actions,
    ),
    skill.description ? el('p', { className: 'desc', textContent: skill.description }) : null,
    scopeChip(skill),
  );
}

// ── 宿主代办 ──────────────────────────────────────────────────────────────

async function reveal(path) {
  try {
    await window.smelt.host('shell.reveal', { path });
  } catch (error) {
    notify(error && error.message ? error.message : String(error));
  }
}

// ── 弹层 ──────────────────────────────────────────────────────────────────

function closeDialog() {
  overlay.hidden = true;
  dialog.replaceChildren();
}

function openDialog(title, body, buttons) {
  dialog.replaceChildren(el('h2', { textContent: title }), ...body, el('div', { className: 'buttons' }, ...buttons));
  overlay.hidden = false;
}

function button(label, className, onClick) {
  return el('button', { className: className || '', textContent: label, onclick: onClick });
}

const cancelButton = () => button('取消', '', closeDialog);

function notify(message) {
  openDialog('操作失败', [el('p', { className: 'error', textContent: message })], [button('知道了', 'primary', closeDialog)]);
}

/// 中性的结果告知：成功、部分成功、无事可做都走这里。
function announce(title, lines) {
  openDialog(
    title,
    lines.map((line) => el('p', { textContent: line })),
    [button('知道了', 'primary', closeDialog)],
  );
}

function failureText(failure) {
  return failure && failure.message ? failure.message : String(failure);
}

/// 「作用范围 + 层级」表单。新建、导入、改范围都用它，问法就只有一种。
function scopeForm(initial, onChange) {
  const value = {
    universal: initial.universal !== false,
    labels: new Set(initial.labels || []),
    projectScope: initial.projectScope === true,
  };
  const notifyChange = () => onChange && onChange(read());
  const read = () => ({
    universal: value.universal,
    labels: [...value.labels],
    project_scope: value.projectScope,
  });

  const boxes = [];
  const syncBoxes = () => {
    for (const box of boxes) box.disabled = value.universal;
  };

  /// 能单独存放 skill 的 agent = 在这个层级有自己目录的那些。
  /// Codex、Grok 只认 .agents/skills，给它们「专属」是做不到的事，别摆出来。
  const hostableAgents = () =>
    state.agents.filter((agent) =>
      value.projectScope ? agent.project_dir !== null : agent.user_dir !== null,
    );

  const kind = (label, universal, hint) =>
    el(
      'label',
      { className: 'choice' },
      el('input', {
        type: 'radio',
        name: 'kind',
        checked: universal === value.universal,
        onchange: () => {
          value.universal = universal;
          syncBoxes();
          notifyChange();
        },
      }),
      label,
      hint ? el('span', { className: 'hint' }, hint) : null,
    );

  const agentBoxes = el('div', { className: 'agent-boxes' });
  // 名单随层级变（同一个 agent 用户级有目录、项目级没有），所以要能重画。
  const renderAgents = () => {
    agentBoxes.replaceChildren();
    boxes.length = 0;
    const hostable = hostableAgents();
    // 勾选里那些在这个层级无处可放的，跟着层级一起清掉，
    // 免得留着一个提交时才报错的选择。
    for (const label of [...value.labels]) {
      if (!hostable.some((agent) => agent.label === label)) value.labels.delete(label);
    }
    // 没在这台机器上用过的排到后面并标注：勾了会凭空建出它的目录，
    // 用户得知道自己在干什么。
    const ordered = [...hostable].sort(
      (left, right) => Number(right.installed) - Number(left.installed),
    );
    for (const agent of ordered) {
      const box = el('input', {
        type: 'checkbox',
        checked: value.labels.has(agent.label),
        disabled: value.universal,
        onchange: (event) => {
          if (event.target.checked) value.labels.add(agent.label);
          else value.labels.delete(agent.label);
          notifyChange();
        },
      });
      boxes.push(box);
      agentBoxes.append(
        el(
          'label',
          { className: 'choice' },
          box,
          agent.label,
          agent.installed ? null : el('span', { className: 'hint' }, '未安装'),
        ),
      );
    }
  };
  renderAgents();

  const level = (label, projectScope, hint) =>
    el(
      'label',
      { className: 'choice' },
      el('input', {
        type: 'radio',
        name: 'level',
        checked: projectScope === value.projectScope,
        // 没有当前项目时项目级无处可放，直接禁用而不是操作完才报错。
        disabled: projectScope && !projectRoot,
        onchange: () => {
          value.projectScope = projectScope;
          renderAgents();
          notifyChange();
        },
      }),
      label,
      hint ? el('span', { className: 'hint' }, hint) : null,
    );

  const nodes = [
    el('p', { className: 'section' }, '作用范围'),
    kind('通用（所有 agent 可见）', true),
    kind('仅指定 agent', false),
    agentBoxes,
    el('p', { className: 'section' }, '层级'),
    level('用户级（所有项目可用）', false),
    level('项目级（只在当前项目）', true, projectRoot ? null : '无当前项目'),
  ];
  return { nodes, read };
}

function openEditDialog(skill) {
  const name = el('input', { type: 'text', value: skill.name });
  const description = el('textarea', { value: skill.description });
  const error = el('p', { className: 'error' });
  const submit = async () => {
    try {
      state = await call('update', { dir: skill.dir, name: name.value, description: description.value });
      closeDialog();
      render();
    } catch (failure) {
      // 校验失败把错误摆在弹窗里，不关闭——用户还要继续改。
      error.textContent = failureText(failure);
    }
  };
  openDialog(
    '编辑 skill',
    [el('label', {}, '名称', name), el('label', {}, '描述', description), error],
    [cancelButton(), button('保存', 'primary', submit)],
  );
  name.focus();
}

function openNewDialog() {
  const name = el('input', { type: 'text', placeholder: 'my-skill' });
  const description = el('textarea', { placeholder: '什么时候该用这个 skill' });
  const error = el('p', { className: 'error' });
  const form = scopeForm({ universal: true, projectScope: false });
  const submit = async () => {
    try {
      state = await call('create', { ...form.read(), name: name.value, description: description.value });
      closeDialog();
      render();
    } catch (failure) {
      error.textContent = failureText(failure);
    }
  };
  openDialog(
    '新建 skill',
    [el('label', {}, '名称', name), el('label', {}, '描述', description), ...form.nodes, error],
    [cancelButton(), button('创建', 'primary', submit)],
  );
  name.focus();
}

/// 改作用范围：先给结论，再把将要发生的文件操作原样列出来。
/// 计划由插件算（`scope.plan`），页面不自己推断磁盘会怎么变。
function openScopeDialog(skill) {
  const preview = el('div', { className: 'preview' });
  const error = el('p', { className: 'error' });
  const apply = button('应用', 'primary', () => {});
  const form = scopeForm(
    {
      universal: skill.placement !== 'agent',
      labels: skill.placement === 'agent' ? skill.agents : [],
      projectScope: skill.project_scope,
    },
    refresh,
  );

  async function refresh(request) {
    const wanted = request || form.read();
    error.textContent = '';
    preview.replaceChildren(el('p', { className: 'hint' }, '正在计算…'));
    let plan;
    try {
      plan = await call('scope.plan', { dir: skill.dir, ...wanted });
    } catch (failure) {
      preview.replaceChildren();
      error.textContent = failureText(failure);
      apply.disabled = true;
      return;
    }
    apply.disabled = false;
    apply.onclick = () => {
      closeDialog();
      run('scope.apply', { dir: skill.dir, ...wanted });
    };
    preview.replaceChildren(
      el('p', { className: 'section' }, plan.steps.length ? '将执行' : '已经是这个状态'),
      ...plan.steps.map((step) => el('div', { className: 'step', textContent: step })),
    );
  }

  openDialog(`作用范围 · ${skill.name}`, [...form.nodes, preview, error], [cancelButton(), apply]);
  refresh();
}

function openDeleteDialog(skill) {
  const remove = (all) => () => {
    closeDialog();
    run('delete', { dir: skill.dir, all });
  };
  // 同名多份时只删一份，列表里那一行不会消失——另一份会顶上来。
  // 不讲清楚的话，用户看到的就是「点了删除什么都没变」。
  if (skill.conflicts.length) {
    const total = skill.conflicts.length + 1;
    return openDialog(
      '删除 skill',
      [
        el('p', {}, `「${skill.name}」在磁盘上有 ${total} 份同名副本：`),
        el(
          'ul',
          { className: 'preview' },
          [skill.display_dir, ...skill.conflicts.map((copy) => copy.display_dir)].map((dir) =>
            el('li', { textContent: dir }),
          ),
        ),
        el('p', { className: 'hint' }, `只删第一份的话，列表里仍会留下「${skill.name}」——剩下的副本会顶上来。`),
      ],
      [
        cancelButton(),
        button('只删第一份', '', remove(false)),
        button(`全部删除（${total} 份）`, 'destructive', remove(true)),
      ],
    );
  }
  openDialog(
    '删除 skill',
    [
      el('p', {}, `确定删除「${skill.name}」？`),
      el('p', {}, `${skill.display_dir} 会被移除，指向它的兼容链接一并清理。`),
    ],
    [cancelButton(), button('删除', 'destructive', remove(false))],
  );
}

/// 同名多份：只让用户选「以哪份为准」，其余进回收目录。
/// 永远不合并内容——两份不同的 SKILL.md 该怎么并只有作者知道。
function openConflictDialog(skill) {
  let keepIndex = 0;
  const copies = [
    { dir: skill.dir, display_dir: skill.display_dir, placement: skill.placement, agent: skill.agent, description: skill.description, modified_ms: skill.modified_ms },
    ...skill.conflicts,
  ];
  const where = (copy) =>
    copy.placement === 'universal' ? '通用' : copy.placement === 'legacy' ? '旧位置' : copy.agent || 'agent';
  const when = (copy) => (copy.modified_ms ? new Date(copy.modified_ms).toLocaleString() : '未知时间');

  const body = [el('p', {}, `「${skill.name}」有 ${copies.length} 份彼此独立的副本，选哪一份为准：`)];
  copies.forEach((copy, index) => {
    body.push(
      el(
        'label',
        { className: 'choice copy' },
        el('input', {
          type: 'radio',
          name: 'copy',
          checked: index === 0,
          onchange: () => {
            keepIndex = index;
          },
        }),
        el(
          'span',
          {},
          el('span', { className: 'chip', textContent: where(copy) }),
          el('code', { textContent: copy.display_dir || copy.dir }),
          el('span', { className: 'hint' }, `改动于 ${when(copy)}`),
          copy.description ? el('span', { className: 'hint' }, copy.description) : null,
        ),
      ),
    );
  });
  body.push(el('p', { className: 'hint' }, '其余副本会移入 ~/.smelt/trash/skills，不会被直接删除。'));

  openDialog('处理同名副本', body, [
    cancelButton(),
    button('确定', 'primary', () => {
      closeDialog();
      run('conflicts.resolve', { dir: skill.dir, keep_index: keepIndex });
    }),
  ]);
}

/// 导入：范围先选、路径后选。反过来的话，用户会在文件选择器退出之后才发现
/// 导入到了不期望的作用域。
function openImportDialog() {
  const form = scopeForm({ universal: true, projectScope: false });
  openDialog(
    '导入 skill',
    [el('p', {}, '选择一个已含 SKILL.md 的目录，整个目录会被复制进来。'), ...form.nodes],
    [
      cancelButton(),
      button('选择目录…', 'primary', () => {
        const request = form.read();
        closeDialog();
        importFrom(request);
      }),
    ],
  );
}

async function importFrom(request) {
  let source;
  try {
    const picked = await window.smelt.host('dialog.pick_directory', {
      prompt: '选择要导入的 skill 目录（需含 SKILL.md）',
    });
    source = picked && picked.path;
  } catch (error) {
    notify(failureText(error));
    return;
  }
  // 用户取消了选择框：什么都不做。
  if (!source) return;

  let info;
  try {
    info = await call('import.inspect', { source });
  } catch (error) {
    notify(failureText(error));
    return;
  }

  // 选了项目级却发现用户级已有同名的：先让用户决定去向，别不知不觉出现两份。
  if (request.project_scope && info.has_global) {
    openDialog(
      '已有同名的用户级 skill',
      [el('p', {}, `「${info.name}」在用户级已经存在。`)],
      [
        cancelButton(),
        button('更新用户级', '', () => {
          closeDialog();
          run('import', { source, ...request, project_scope: false });
        }),
        button('保留项目覆盖', 'primary', () => {
          closeDialog();
          run('import', { source, ...request });
        }),
      ],
    );
    return;
  }
  run('import', { source, ...request });
}

// ── 启动 ──────────────────────────────────────────────────────────────────

$('refresh').addEventListener('click', async (event) => {
  // 磁盘没变化时列表长得一模一样，不给反馈的话又是一次「点了没反应」。
  const target = event.currentTarget;
  if (target.disabled) return;
  target.disabled = true;
  target.textContent = '扫描中…';
  try {
    await load();
  } finally {
    target.textContent = '已刷新';
    setTimeout(() => {
      target.textContent = '刷新';
      target.disabled = false;
    }, 900);
  }
});
$('new').addEventListener('click', openNewDialog);
$('import').addEventListener('click', openImportDialog);
overlay.addEventListener('mousedown', (event) => {
  if (event.target === overlay) closeDialog();
});
document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape' && !overlay.hidden) closeDialog();
});
window.smelt.on((message) => {
  if (message && message.kind === 'context') applyContext(message.context);
});

// skill 是磁盘上的目录，谁都可以在 smelt 之外新增/删除（手动 clone、别的工具装的）。
// 面板没有文件监听，只在启动时扫一次的话，手动装完的 skill 要重启才看得到。
// 重新看到面板时补扫一次，配合表头的「刷新」把这件事补齐。
function refreshIfVisible() {
  // 弹窗开着说明用户正在操作某个具体 skill，这时重画会把它脚下的数据换掉。
  if (!booted || document.hidden || !overlay.hidden) return;
  load();
}
document.addEventListener('visibilitychange', refreshIfVisible);
window.addEventListener('focus', refreshIfVisible);

(async () => {
  try {
    applyContext(await window.smelt.host('ui.context'));
  } catch (error) {
    // 拿不到上下文不是致命的：没有当前项目，面板就只显示用户级。
    console.warn('[skills] 读取宿主上下文失败', error);
  }
  booted = true;
  await load();
})();
