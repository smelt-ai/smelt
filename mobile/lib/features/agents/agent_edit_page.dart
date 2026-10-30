import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';
import '../dirs/dir_browser.dart';
import 'agents_board.dart';
import 'agents_widgets.dart';

/// 编辑页关掉时的结果：删掉了的话，详情页跟着退。
enum AgentEditResult { saved, deleted }

/// 新建或编辑一个智能体。[agentId] 为空是新建。
///
/// 模型、插件、MCP 的选项来自这台机器的能力库；取不到时照样能改名字和指令，并说明原因。
/// 保存只发改了的字段（见 `crate::agents`），桌面同时改别的字段不会被盖掉。
class AgentEditPage extends StatefulWidget {
  const AgentEditPage({
    super.key,
    required this.board,
    required this.runtimeId,
    this.agentId,
  });

  final AgentsBoard board;
  final String runtimeId;
  final String? agentId;

  @override
  State<AgentEditPage> createState() => _AgentEditPageState();
}

class _AgentEditPageState extends State<AgentEditPage> {
  final _name = TextEditingController();
  final _prompt = TextEditingController();

  AgentEditor? _editor;
  String? _loadError;

  List<String> _plugins = const [];
  List<String> _mcpServers = const [];
  List<String> _folders = const [];
  List<String> _links = const [];
  String _modelProvider = '';
  String _modelId = '';
  bool _memoryEnabled = true;

  bool _saving = false;
  bool _deleting = false;

  bool get _creating => widget.agentId == null;

  @override
  void initState() {
    super.initState();
    _name.addListener(_changed);
    _prompt.addListener(_changed);
    unawaited(_load());
  }

  @override
  void dispose() {
    _name.dispose();
    _prompt.dispose();
    super.dispose();
  }

  void _changed() => setState(() {});

  /// 手机上点输入框外面默认不失焦，键盘就一直挡着。
  void _unfocus(PointerDownEvent _) => FocusScope.of(context).unfocus();

  Future<void> _load() async {
    setState(() => _loadError = null);
    try {
      final editor = await AppScope.of(
        context,
      ).backend.agentEditor(widget.runtimeId, agentId: widget.agentId);
      if (!mounted) return;
      final draft = editor.draft;
      _name.text = draft.name;
      _prompt.text = draft.prompt;
      setState(() {
        _editor = editor;
        _plugins = draft.plugins;
        _mcpServers = draft.mcpServers;
        _folders = draft.contextFolders;
        _links = draft.contextLinks;
        _modelProvider = draft.modelProvider;
        _modelId = draft.modelId;
        _memoryEnabled = draft.memoryEnabled;
      });
    } catch (error) {
      if (mounted) setState(() => _loadError = describeError(error));
    }
  }

  AgentDraft get _draft => AgentDraft(
    name: _name.text,
    prompt: _prompt.text,
    plugins: _plugins,
    mcpServers: _mcpServers,
    contextFolders: _folders,
    contextLinks: _links,
    modelProvider: _modelProvider,
    modelId: _modelId,
    memoryEnabled: _memoryEnabled,
  );

  bool get _dirty {
    final editor = _editor;
    return editor != null && _draft != editor.draft;
  }

  bool get _canSave =>
      _editor != null &&
      !_saving &&
      !_deleting &&
      _name.text.trim().isNotEmpty
      // 新建时名字填了就能存；编辑时要真改了什么。
      &&
      (_creating || _dirty);

  Future<void> _save() async {
    final editor = _editor;
    if (editor == null || !_canSave) return;
    final backend = AppScope.of(context).backend;
    final messenger = ScaffoldMessenger.of(context);
    final l10n = L10n.of(context);
    setState(() => _saving = true);
    try {
      final agentId = widget.agentId;
      if (agentId == null) {
        await backend.createAgent(widget.runtimeId, _draft);
      } else {
        await backend.updateAgent(
          widget.runtimeId,
          agentId,
          original: editor.draft,
          edited: _draft,
        );
      }
      unawaited(widget.board.reload(widget.runtimeId));
      if (mounted) Navigator.of(context).pop(AgentEditResult.saved);
    } catch (error) {
      messenger.showSnackBar(
        SnackBar(content: Text(l10n.agentEditSaveFailed(describeError(error)))),
      );
      if (mounted) setState(() => _saving = false);
    }
  }

  Future<void> _delete() async {
    final agentId = widget.agentId;
    if (agentId == null) return;
    final l10n = L10n.of(context);
    final model = AppScope.of(context);
    final messenger = ScaffoldMessenger.of(context);
    final machine = model.fleet.runtime(widget.runtimeId)?.name ?? '';
    final name = _editor?.draft.name ?? '';
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.agentEditDeleteTitle(name)),
        content: Text(l10n.agentEditDeleteBody(machine)),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.commonCancel),
          ),
          GalleyButton.danger(
            label: l10n.commonDelete,
            onPressed: () => Navigator.pop(context, true),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    setState(() => _deleting = true);
    try {
      await model.backend.deleteAgent(widget.runtimeId, agentId);
      unawaited(widget.board.reload(widget.runtimeId));
      if (mounted) Navigator.of(context).pop(AgentEditResult.deleted);
    } catch (error) {
      // 还有自动化在用它时运行时会拒绝，原话转给用户（「请先删除关联的 2 条自动化」）。
      messenger.showSnackBar(
        SnackBar(
          content: Text(l10n.agentEditDeleteFailed(describeError(error))),
        ),
      );
      if (mounted) setState(() => _deleting = false);
    }
  }

  /// 有没保存的改动时，离开前问一句。
  Future<void> _confirmLeave() async {
    final l10n = L10n.of(context);
    final navigator = Navigator.of(context);
    final discard = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.agentEditDiscardTitle),
        content: Text(l10n.agentEditDiscardBody),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: Text(l10n.commonCancel),
          ),
          GalleyButton.danger(
            label: l10n.agentEditDiscard,
            onPressed: () => Navigator.pop(context, true),
          ),
        ],
      ),
    );
    if (discard == true) navigator.pop();
  }

  Future<void> _pickModel() async {
    final editor = _editor;
    if (editor == null) return;
    final picked = await showModalBottomSheet<(String, String)>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      builder: (context) => _ModelSheet(
        groups: editor.models,
        provider: _modelProvider,
        id: _modelId,
      ),
    );
    if (picked == null) return;
    setState(() {
      _modelProvider = picked.$1;
      _modelId = picked.$2;
    });
  }

  Future<void> _addFolder() async {
    final path = await Navigator.of(context).push<String>(
      MaterialPageRoute(
        builder: (_) => _FolderPickPage(runtimeId: widget.runtimeId),
      ),
    );
    if (path == null || _folders.contains(path)) return;
    setState(() => _folders = [..._folders, path]);
  }

  Future<void> _addLink() async {
    final link = await showDialog<String>(
      context: context,
      builder: (_) => const _LinkDialog(),
    );
    if (link == null || _links.contains(link)) return;
    setState(() => _links = [..._links, link]);
  }

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final runtime = model.fleet.runtime(widget.runtimeId);
    final busy = _saving || _deleting;
    return PopScope(
      canPop: !_dirty && !busy,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop && !busy) unawaited(_confirmLeave());
      },
      child: Scaffold(
        backgroundColor: c.ground,
        appBar: AppBar(
          backgroundColor: c.ground,
          title: DetailTitle(
            title: _creating ? l10n.agentEditNewTitle : l10n.agentEditTitle,
            runtime: runtime,
          ),
          actions: [
            Padding(
              padding: const EdgeInsetsDirectional.only(end: Space.sm),
              child: TextButton(
                onPressed: _canSave ? _save : null,
                child: Text(_saving ? l10n.agentEditSaving : l10n.commonSave),
              ),
            ),
          ],
        ),
        body: _body(context),
      ),
    );
  }

  Widget _body(BuildContext context) {
    final l10n = L10n.of(context);
    final editor = _editor;
    if (editor == null) {
      final error = _loadError;
      if (error == null) {
        return const Center(child: CircularProgressIndicator.adaptive());
      }
      return Center(
        child: EmptyNote(
          icon: LucideIcons.cloudOff,
          title: l10n.agentEditLoadFailed,
          body: error,
          action: GalleyButton.solid(label: l10n.commonRetry, onPressed: _load),
        ),
      );
    }
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final enabled = !_saving && !_deleting;
    final choicesError = editor.choicesError;
    return ListView(
      // 键盘收不起来的输入法（没有收起键）也能走：拖动列表或点输入框外面都收起。
      keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
      padding: EdgeInsets.only(
        bottom: MediaQuery.paddingOf(context).bottom + Space.xxl,
      ),
      children: [
        ShelfHeader(title: l10n.agentEditName, first: true),
        _Field(
          child: TextField(
            controller: _name,
            enabled: enabled,
            autofocus: _creating,
            textInputAction: TextInputAction.next,
            onTapOutside: _unfocus,
            style: t.body.copyWith(color: c.ink),
            decoration: InputDecoration(hintText: l10n.agentEditNameHint),
          ),
        ),
        ShelfHeader(title: l10n.agentsPrompt),
        _Field(
          child: TextField(
            controller: _prompt,
            enabled: enabled,
            minLines: 6,
            maxLines: null,
            keyboardType: TextInputType.multiline,
            onTapOutside: _unfocus,
            style: t.body.copyWith(color: c.ink),
            decoration: InputDecoration(hintText: l10n.agentEditPromptHint),
          ),
        ),
        if (choicesError != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Space.page + Space.xs,
              Space.section,
              Space.page + Space.xs,
              0,
            ),
            child: Text(
              l10n.agentEditChoicesFailed(choicesError),
              style: t.note.copyWith(color: c.danger),
            ),
          ),
        ShelfHeader(title: l10n.agentEditModel),
        Shelf(
          children: [
            ShelfRow(
              leading: Icon(
                LucideIcons.cpu,
                size: Strokes.icon,
                color: c.inkMuted,
              ),
              title: Text(
                agentModelLabel(l10n, editor.models, _modelProvider, _modelId),
              ),
              trailing: _chevron(c),
              onTap: enabled ? _pickModel : null,
            ),
          ],
        ),
        ShelfHeader(title: l10n.agentsPlugins),
        Shelf(children: _pluginRows(context, editor, enabled)),
        ShelfHeader(title: l10n.agentEditMcp),
        Shelf(children: _mcpRows(context, editor, enabled)),
        ShelfHeader(title: l10n.agentEditFolders),
        Shelf(
          children: [
            for (final folder in _folders)
              _RemovableRow(
                icon: LucideIcons.folder,
                text: folder,
                onRemove: enabled
                    ? () => setState(
                        () => _folders = [
                          for (final f in _folders)
                            if (f != folder) f,
                        ],
                      )
                    : null,
              ),
            _AddRow(
              label: l10n.agentEditAddFolder,
              onTap: enabled ? _addFolder : null,
            ),
          ],
        ),
        ShelfHeader(title: l10n.agentEditLinks),
        Shelf(
          children: [
            for (final link in _links)
              _RemovableRow(
                icon: LucideIcons.link,
                text: link,
                onRemove: enabled
                    ? () => setState(
                        () => _links = [
                          for (final l in _links)
                            if (l != link) l,
                        ],
                      )
                    : null,
              ),
            _AddRow(
              label: l10n.agentEditAddLink,
              onTap: enabled ? _addLink : null,
            ),
          ],
        ),
        ShelfHeader(title: l10n.agentEditMemory),
        Shelf(
          children: [
            ShelfRow(
              title: Text(l10n.agentEditMemory),
              subtitle: Text(l10n.agentEditMemoryHint, maxLines: 3),
              trailing: Switch(
                value: _memoryEnabled,
                onChanged: enabled
                    ? (value) => setState(() => _memoryEnabled = value)
                    : null,
              ),
            ),
          ],
        ),
        if (!_creating)
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Space.page,
              Space.section,
              Space.page,
              0,
            ),
            child: GalleyButton.danger(
              expand: true,
              icon: LucideIcons.trash2,
              label: l10n.agentEditDelete,
              onPressed: enabled ? _delete : null,
            ),
          ),
      ],
    );
  }

  List<Widget> _pluginRows(
    BuildContext context,
    AgentEditor editor,
    bool enabled,
  ) {
    final l10n = L10n.of(context);
    final known = {for (final p in editor.plugins) p.id};
    // 勾着、但这台机器上已经没有的：列出来，能取消，不悄悄丢掉。
    final missing = _plugins.where((id) => !known.contains(id));
    void toggle(String id, bool on) => setState(
      () => _plugins = on
          ? [..._plugins, id]
          : [
              for (final p in _plugins)
                if (p != id) p,
            ],
    );
    final rows = <Widget>[
      for (final plugin in editor.plugins)
        _CheckRow(
          icon: pluginIcon(plugin.kind),
          title: plugin.name,
          subtitle: plugin.broken ?? _nonEmpty(plugin.description),
          danger: plugin.broken != null,
          checked: _plugins.contains(plugin.id),
          // 坏了的不能新勾，已经勾着的可以取消。
          onChanged:
              enabled && (plugin.broken == null || _plugins.contains(plugin.id))
              ? (on) => toggle(plugin.id, on)
              : null,
        ),
      for (final id in missing)
        _CheckRow(
          icon: LucideIcons.circleSlash,
          title: id,
          subtitle: l10n.agentEditMissing,
          danger: true,
          checked: true,
          onChanged: enabled ? (on) => toggle(id, on) : null,
        ),
    ];
    if (rows.isEmpty) return [NoteRow(text: l10n.agentEditPluginsNone)];
    return rows;
  }

  List<Widget> _mcpRows(
    BuildContext context,
    AgentEditor editor,
    bool enabled,
  ) {
    final l10n = L10n.of(context);
    final known = {for (final s in editor.mcpServers) s.name};
    final missing = _mcpServers.where((name) => !known.contains(name));
    void toggle(String name, bool on) => setState(
      () => _mcpServers = on
          ? [..._mcpServers, name]
          : [
              for (final s in _mcpServers)
                if (s != name) s,
            ],
    );
    final rows = <Widget>[
      for (final server in editor.mcpServers)
        _CheckRow(
          icon: LucideIcons.server,
          title: server.name,
          subtitle: server.enabled
              ? _nonEmpty(server.description ?? '')
              : l10n.agentEditMcpDisabled,
          checked: _mcpServers.contains(server.name),
          onChanged: enabled ? (on) => toggle(server.name, on) : null,
        ),
      for (final name in missing)
        _CheckRow(
          icon: LucideIcons.circleSlash,
          title: name,
          subtitle: l10n.agentEditMissing,
          danger: true,
          checked: true,
          onChanged: enabled ? (on) => toggle(name, on) : null,
        ),
    ];
    if (rows.isEmpty) return [NoteRow(text: l10n.agentEditMcpNone)];
    return rows;
  }

  Widget _chevron(GalleyColors c) => Icon(
    LucideIcons.chevronRight,
    size: Strokes.iconSmall,
    color: c.inkFaint,
  );
}

String? _nonEmpty(String text) => text.trim().isEmpty ? null : text.trim();

/// 一个输入框：用主题的输入框样式（白底、描边、内边距），左右和搁板对齐。不再套搁板，
/// 免得白板里再嵌一个框。
class _Field extends StatelessWidget {
  const _Field({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: Space.page),
    child: child,
  );
}

/// 能勾的一行：插件、MCP。
class _CheckRow extends StatelessWidget {
  const _CheckRow({
    required this.icon,
    required this.title,
    required this.checked,
    required this.onChanged,
    this.subtitle,
    this.danger = false,
  });

  final IconData icon;
  final String title;
  final String? subtitle;
  final bool danger;
  final bool checked;
  final ValueChanged<bool>? onChanged;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final sub = subtitle;
    return ShelfRow(
      leading: Icon(icon, size: Strokes.icon, color: c.inkMuted),
      title: Text(title),
      subtitle: sub == null
          ? null
          : Text(sub, style: danger ? t.meta.copyWith(color: c.danger) : null),
      semanticLabel: title,
      trailing: Checkbox(
        value: checked,
        onChanged: onChanged == null ? null : (v) => onChanged!(v ?? false),
      ),
      onTap: onChanged == null ? null : () => onChanged!(!checked),
    );
  }
}

/// 一项绑定的目录或链接：全文展示，尾部移除。
class _RemovableRow extends StatelessWidget {
  const _RemovableRow({
    required this.icon,
    required this.text,
    required this.onRemove,
  });

  final IconData icon;
  final String text;
  final VoidCallback? onRemove;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Padding(
      padding: const EdgeInsetsDirectional.only(start: Space.lg),
      child: Row(
        children: [
          Icon(icon, size: Strokes.iconSmall, color: c.inkMuted),
          const SizedBox(width: Space.md),
          Expanded(
            child: Text(text, style: t.body.copyWith(color: c.ink)),
          ),
          IconButton(
            tooltip: L10n.of(context).agentEditRemove,
            icon: const Icon(LucideIcons.x),
            onPressed: onRemove,
          ),
        ],
      ),
    );
  }
}

class _AddRow extends StatelessWidget {
  const _AddRow({required this.label, required this.onTap});

  final String label;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return ShelfRow(
      leading: Icon(LucideIcons.plus, size: Strokes.icon, color: c.inkMuted),
      title: Text(label),
      onTap: onTap,
    );
  }
}

/// 选模型：跟随默认在最前，下面按 provider 分组。
class _ModelSheet extends StatelessWidget {
  const _ModelSheet({
    required this.groups,
    required this.provider,
    required this.id,
  });

  final List<ModelGroup> groups;
  final String provider;
  final String id;

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final c = GalleyColors.of(context);
    final followDefault = provider.isEmpty || id.isEmpty;
    Widget check(bool on) => on
        ? Icon(LucideIcons.check, size: Strokes.icon, color: c.ink)
        : const SizedBox(width: Strokes.icon);
    return ConstrainedBox(
      constraints: BoxConstraints(
        maxHeight: MediaQuery.sizeOf(context).height * 0.85,
      ),
      child: ListView(
        shrinkWrap: true,
        padding: EdgeInsets.only(
          bottom: MediaQuery.paddingOf(context).bottom + Space.lg,
        ),
        children: [
          ShelfHeader(title: l10n.agentEditModel, first: true),
          HairlineRows(
            children: [
              ShelfRow(
                title: Text(l10n.agentEditModelDefault),
                trailing: check(followDefault),
                onTap: () => Navigator.pop(context, ('', '')),
              ),
            ],
          ),
          for (final group in groups) ...[
            ShelfHeader(title: group.providerName),
            HairlineRows(
              children: [
                for (final option in group.models)
                  ShelfRow(
                    title: Text(option.label),
                    trailing: check(
                      option.provider == provider && option.id == id,
                    ),
                    onTap: () =>
                        Navigator.pop(context, (option.provider, option.id)),
                  ),
              ],
            ),
          ],
        ],
      ),
    );
  }
}

/// 在智能体那台机器上选一个目录绑上去。返回选中的绝对路径。
class _FolderPickPage extends StatefulWidget {
  const _FolderPickPage({required this.runtimeId});

  final String runtimeId;

  @override
  State<_FolderPickPage> createState() => _FolderPickPageState();
}

class _FolderPickPageState extends State<_FolderPickPage> {
  late final DirBrowser _browser;

  @override
  void initState() {
    super.initState();
    _browser = DirBrowser(AppScope.of(context).backend, widget.runtimeId);
    unawaited(_browser.start());
  }

  @override
  void dispose() {
    _browser.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    return ListenableBuilder(
      listenable: _browser,
      // 系统返回先退回上一层目录，退到起点才离开。
      builder: (context, _) => PopScope(
        canPop: !_browser.canGoBack,
        onPopInvokedWithResult: (didPop, _) {
          if (!didPop) _browser.back();
        },
        child: Scaffold(
          backgroundColor: c.ground,
          appBar: AppBar(backgroundColor: c.ground),
          body: DirBrowserView(
            browser: _browser,
            actionLabel: (listing) => l10n.agentEditFolderAdd(listing.name),
            onAction: (listing) => Navigator.of(context).pop(listing.path),
          ),
        ),
      ),
    );
  }
}

class _LinkDialog extends StatefulWidget {
  const _LinkDialog();

  @override
  State<_LinkDialog> createState() => _LinkDialogState();
}

class _LinkDialogState extends State<_LinkDialog> {
  final _text = TextEditingController();

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  void _save() {
    final link = _text.text.trim();
    if (link.isNotEmpty) Navigator.pop(context, link);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    return AlertDialog(
      title: Text(l10n.agentEditAddLink),
      content: TextField(
        controller: _text,
        autofocus: true,
        keyboardType: TextInputType.url,
        autocorrect: false,
        textInputAction: TextInputAction.done,
        onSubmitted: (_) => _save(),
        decoration: const InputDecoration(hintText: 'https://'),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: Text(l10n.commonCancel),
        ),
        ListenableBuilder(
          listenable: _text,
          builder: (context, _) => FilledButton(
            onPressed: _text.text.trim().isEmpty ? null : _save,
            child: Text(l10n.commonAdd),
          ),
        ),
      ],
    );
  }
}
