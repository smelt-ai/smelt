/// 配对：全屏扫码页（取景框、手电筒、手动粘贴配对串），扫到就地配对。
///
/// 配对是「加一台机器」，不是「切到那台机器」：内核保证配对不影响其它机器的连接，
/// 这里也不出现任何「当前机器」的说法。成功后回到上一页，提示已连上哪台。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import '../../data/backend.dart';
import '../../data/device_name.dart';
import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../theme/theme.dart';
import '../../ui/buttons.dart';
import 'machine_text.dart';

/// 打开扫码页配对一台机器（新机器，或被撤销后重新配对）。成功后在调用方所在的页面
/// 提示「已连上 <机器名>」；用户中途关掉就什么都不做。
Future<void> startPairing(BuildContext context) async {
  final messenger = ScaffoldMessenger.of(context);
  final l10n = L10n.of(context);
  // 压到根导航：平板上从详情栏里点「重新配对」也是全屏扫码，不挤在右栏里。
  final record = await Navigator.of(context, rootNavigator: true)
      .push<RuntimeRecord>(
        MaterialPageRoute(
          fullscreenDialog: true,
          builder: (_) => const PairingPage(),
        ),
      );
  if (record == null) return;
  messenger
    ..clearSnackBars()
    ..showSnackBar(SnackBar(content: Text(l10n.machinesPaired(record.name))));
}

/// 本地校验配对串（不联网）。合法返回 null，否则返回给人看的原因。
String? checkPairingCode(Backend backend, L10n l10n, String uri) {
  try {
    backend.parsePairing(uri);
    return null;
  } on SmeltError catch (error) {
    // 版本不兼容（配对串格式更新了）要说清楚是版本问题；其余一律是「这不是配对码」，
    // 用户扫到的多半是别的二维码，内核的英文细节对他没用。
    return error.kind == SmeltErrorKind.incompatible
        ? pairingFailureText(l10n, error)
        : l10n.machinesPairNotACode;
  } catch (_) {
    return l10n.machinesPairNotACode;
  }
}

enum _Stage { scanning, pairing, failed }

/// 取景页的深色主题，建一次就够，不在每次重建时重算。
final _cameraTheme = buildGalleyTheme(Brightness.dark);

/// 扫到不是配对码时那句提示停留多久：占的是指引文案的位置，不收回的话用户扫错一次
/// 之后一直盯着一条过期的提示，反而不知道该怎么对准。
const _hintLinger = Duration(seconds: 4);

class PairingPage extends StatefulWidget {
  const PairingPage({super.key});

  @override
  State<PairingPage> createState() => _PairingPageState();
}

class _PairingPageState extends State<PairingPage> {
  final _scanner = MobileScannerController(
    detectionSpeed: DetectionSpeed.noDuplicates,
    formats: const [BarcodeFormat.qrCode],
  );

  _Stage _stage = _Stage.scanning;
  String? _hint;
  Timer? _hintTimer;
  String? _busyLabel;
  String? _failure;

  @override
  void dispose() {
    _hintTimer?.cancel();
    _scanner.dispose();
    super.dispose();
  }

  void _flash(String message) {
    setState(() => _hint = message);
    _hintTimer?.cancel();
    _hintTimer = Timer(_hintLinger, () {
      if (mounted) setState(() => _hint = null);
    });
  }

  void _onDetect(BarcodeCapture capture) {
    if (_stage != _Stage.scanning) return;
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    for (final barcode in capture.barcodes) {
      final raw = barcode.rawValue?.trim();
      if (raw == null || raw.isEmpty) continue;
      final problem = checkPairingCode(model.backend, l10n, raw);
      if (problem != null) {
        _flash(problem);
        continue;
      }
      unawaited(_pair(raw));
      return;
    }
  }

  /// [uri] 已经本地校验过。
  Future<void> _pair(String uri) async {
    final model = AppScope.of(context);
    final l10n = L10n.of(context);
    final target = model.backend.parsePairing(uri);
    // 扫的是已登记的机器（被撤销后重新配对）就说出它的名字，内核会沿用原来的登记。
    final known = model.fleet.value.runtimes
        .where((r) => r.record.endpointId == target.endpointId)
        .firstOrNull;
    _hintTimer?.cancel();
    setState(() {
      _stage = _Stage.pairing;
      _hint = null;
      _failure = null;
      _busyLabel = known == null
          ? l10n.machinesPairBusyNew
          : l10n.machinesPairBusyAgain(known.name);
    });
    // 配对期间不再识别：配对码一次性，同一张码再扫一遍只会得到「已用过」。
    unawaited(_scanner.pause().catchError((Object _) {}));
    try {
      final record = await model.backend.pair(uri, await defaultDeviceName());
      if (!mounted) return;
      Navigator.of(context).pop(record);
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _stage = _Stage.failed;
        _failure = pairingFailureText(l10n, error);
      });
    }
  }

  void _scanAgain() {
    setState(() {
      _stage = _Stage.scanning;
      _failure = null;
    });
    // 相机本来就不可用（没权限、没摄像头）时 start 会抛，错误由预览区自己画。
    unawaited(_scanner.start().catchError((Object _) {}));
  }

  Future<void> _paste() async {
    final backend = AppScope.of(context).backend;
    final l10n = L10n.of(context);
    final uri = await showModalBottomSheet<String>(
      context: context,
      isScrollControlled: true,
      builder: (_) =>
          _PasteSheet(check: (text) => checkPairingCode(backend, l10n, text)),
    );
    if (uri == null || !mounted) return;
    await _pair(uri);
  }

  @override
  Widget build(BuildContext context) {
    // 取景页一律用深色：相机画面周围亮底会晃眼，系统相机也是这样。
    return Theme(
      data: _cameraTheme,
      child: Builder(builder: _buildPage),
    );
  }

  Widget _buildPage(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final closeLabel = MaterialLocalizations.of(context).closeButtonTooltip;
    return Scaffold(
      backgroundColor: c.ground,
      extendBodyBehindAppBar: true,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        leading: IconButton(
          tooltip: closeLabel,
          icon: const Icon(LucideIcons.x),
          onPressed: () => Navigator.of(context).maybePop(),
        ),
        title: Text(l10n.machinesPairTitle),
        actions: [
          if (_stage == _Stage.scanning) _TorchButton(scanner: _scanner),
          const SizedBox(width: Space.xs),
        ],
      ),
      body: Column(
        children: [
          Expanded(
            child: Stack(
              fit: StackFit.expand,
              children: [
                MobileScanner(
                  controller: _scanner,
                  onDetect: _onDetect,
                  errorBuilder: (context, error) => _CameraProblem(
                    text:
                        error.errorCode ==
                            MobileScannerErrorCode.permissionDenied
                        ? l10n.machinesCameraDenied
                        : l10n.machinesCameraUnavailable,
                  ),
                ),
                IgnorePointer(
                  child: CustomPaint(
                    painter: _ViewfinderPainter(
                      scrim: c.scrim,
                      corner: c.ink,
                      dimmed: _stage != _Stage.scanning,
                    ),
                  ),
                ),
              ],
            ),
          ),
          _Panel(
            stage: _stage,
            hint: _hint,
            busyLabel: _busyLabel,
            failure: _failure,
            onPaste: _paste,
            onScanAgain: _scanAgain,
          ),
        ],
      ),
    );
  }
}

class _TorchButton extends StatelessWidget {
  const _TorchButton({required this.scanner});

  final MobileScannerController scanner;

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    return ValueListenableBuilder<MobileScannerState>(
      valueListenable: scanner,
      builder: (context, state, _) {
        if (!state.isRunning || state.torchState == TorchState.unavailable) {
          return const SizedBox.shrink();
        }
        final on = state.torchState == TorchState.on;
        return IconButton(
          tooltip: on ? l10n.machinesPairTorchOff : l10n.machinesPairTorchOn,
          isSelected: on,
          icon: Icon(on ? LucideIcons.flashlight : LucideIcons.flashlightOff),
          onPressed: scanner.toggleTorch,
        );
      },
    );
  }
}

class _CameraProblem extends StatelessWidget {
  const _CameraProblem({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return ColoredBox(
      color: c.ground,
      child: Center(
        child: Padding(
          padding: const EdgeInsets.all(Space.xxl),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(LucideIcons.cameraOff, color: c.inkMuted),
              const SizedBox(height: Space.md),
              Text(
                text,
                textAlign: TextAlign.center,
                style: t.body.copyWith(color: c.ink),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 取景框：框外压暗，框角四道折线。配对中、失败时整体再压暗一些，表示不在识别。
class _ViewfinderPainter extends CustomPainter {
  _ViewfinderPainter({
    required this.scrim,
    required this.corner,
    required this.dimmed,
  });

  final Color scrim;
  final Color corner;
  final bool dimmed;

  static const _cornerLength = 28.0;
  static const _cornerStroke = 3.0;

  @override
  void paint(Canvas canvas, Size size) {
    final side = (size.shortestSide * 0.66).clamp(180.0, 300.0);
    // 框略偏上：下方有面板，视觉中心本来就在上面。
    final center = Offset(size.width / 2, size.height * 0.46);
    final rect = Rect.fromCenter(center: center, width: side, height: side);
    final hole = RRect.fromRectAndRadius(rect, Radii.shelf);

    final outside = Path()
      ..fillType = PathFillType.evenOdd
      ..addRect(Offset.zero & size)
      ..addRRect(hole);
    canvas.drawPath(outside, Paint()..color = scrim);
    if (dimmed) {
      canvas.drawRRect(hole, Paint()..color = scrim);
    }

    final paint = Paint()
      ..color = corner
      ..strokeWidth = _cornerStroke
      ..style = PaintingStyle.stroke
      ..strokeCap = StrokeCap.round;
    const l = _cornerLength;
    final r = Radii.shelf.x;
    void bracket(Offset at, double dx, double dy) {
      canvas.drawPath(
        Path()
          ..moveTo(at.dx, at.dy + dy * l)
          ..lineTo(at.dx, at.dy + dy * r)
          ..quadraticBezierTo(at.dx, at.dy, at.dx + dx * r, at.dy)
          ..lineTo(at.dx + dx * l, at.dy),
        paint,
      );
    }

    bracket(rect.topLeft, 1, 1);
    bracket(rect.topRight, -1, 1);
    bracket(rect.bottomLeft, 1, -1);
    bracket(rect.bottomRight, -1, -1);
  }

  @override
  bool shouldRepaint(_ViewfinderPainter old) =>
      old.scrim != scrim || old.corner != corner || old.dimmed != dimmed;
}

/// 取景框下面的面板：指引 / 进行中 / 失败原因，加上手动粘贴的入口。
class _Panel extends StatelessWidget {
  const _Panel({
    required this.stage,
    required this.hint,
    required this.busyLabel,
    required this.failure,
    required this.onPaste,
    required this.onScanAgain,
  });

  final _Stage stage;
  final String? hint;
  final String? busyLabel;
  final String? failure;
  final VoidCallback onPaste;
  final VoidCallback onScanAgain;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);

    final Widget body = switch (stage) {
      _Stage.scanning => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Semantics(
            liveRegion: true,
            child: Text(
              hint ?? l10n.machinesPairGuide,
              textAlign: TextAlign.center,
              style: t.body.copyWith(color: hint == null ? c.ink : c.danger),
            ),
          ),
          const SizedBox(height: Space.lg),
          GalleyButton.quiet(
            label: l10n.machinesPairPaste,
            icon: LucideIcons.clipboardPaste,
            expand: true,
            onPressed: onPaste,
          ),
        ],
      ),
      _Stage.pairing => Semantics(
        liveRegion: true,
        child: Column(
          children: [
            const SizedBox.square(
              dimension: Strokes.icon,
              child: CircularProgressIndicator.adaptive(strokeWidth: 2),
            ),
            const SizedBox(height: Space.md),
            Text(
              busyLabel ?? '',
              textAlign: TextAlign.center,
              style: t.headline.copyWith(color: c.ink),
            ),
            const SizedBox(height: Space.xs),
            Text(
              l10n.machinesPairBusyBody,
              textAlign: TextAlign.center,
              style: t.meta.copyWith(color: c.inkMuted),
            ),
            // 和其它两种状态的按钮行差不多高，面板不跟着跳。
            const SizedBox(height: Space.lg),
          ],
        ),
      ),
      _Stage.failed => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Semantics(
            liveRegion: true,
            child: Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                Icon(
                  LucideIcons.circleAlert,
                  size: Strokes.icon,
                  color: c.danger,
                ),
                const SizedBox(width: Space.sm),
                Flexible(
                  child: Text(
                    l10n.machinesPairFailed,
                    style: t.headline.copyWith(color: c.ink),
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(height: Space.xs),
          Text(
            failure ?? '',
            textAlign: TextAlign.center,
            style: t.body.copyWith(color: c.inkMuted),
          ),
          const SizedBox(height: Space.lg),
          Row(
            children: [
              Expanded(
                child: GalleyButton.quiet(
                  label: l10n.machinesPairPaste,
                  expand: true,
                  onPressed: onPaste,
                ),
              ),
              const SizedBox(width: Space.sm),
              Expanded(
                child: GalleyButton.solid(
                  label: l10n.machinesPairScanAgain,
                  icon: LucideIcons.scanLine,
                  expand: true,
                  onPressed: onScanAgain,
                ),
              ),
            ],
          ),
        ],
      ),
    };

    return Material(
      color: c.ground,
      child: SafeArea(
        top: false,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(
            Space.xxl,
            Space.xl,
            Space.xxl,
            Space.lg,
          ),
          child: body,
        ),
      ),
    );
  }
}

/// 手动粘贴配对串：相机用不了、或配对串是从别处复制来的时候用。提交前先本地校验。
class _PasteSheet extends StatefulWidget {
  const _PasteSheet({required this.check});

  final String? Function(String uri) check;

  @override
  State<_PasteSheet> createState() => _PasteSheetState();
}

class _PasteSheetState extends State<_PasteSheet> {
  final _text = TextEditingController();
  String? _error;

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  Future<void> _fromClipboard() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text?.trim();
    if (text == null || text.isEmpty || !mounted) return;
    _text.text = text;
    _text.selection = TextSelection.collapsed(offset: text.length);
    setState(() => _error = null);
  }

  void _submit() {
    final uri = _text.text.trim();
    if (uri.isEmpty) return;
    final problem = widget.check(uri);
    if (problem != null) {
      setState(() => _error = problem);
      return;
    }
    Navigator.of(context).pop(uri);
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    return Padding(
      padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
      child: SafeArea(
        top: false,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(
            Space.page,
            0,
            Space.page,
            Space.lg,
          ),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                l10n.machinesPairPaste,
                style: t.title.copyWith(color: c.ink),
              ),
              const SizedBox(height: Space.md),
              TextField(
                controller: _text,
                autofocus: true,
                minLines: 1,
                maxLines: 4,
                keyboardType: TextInputType.url,
                autocorrect: false,
                enableSuggestions: false,
                textInputAction: TextInputAction.done,
                style: t.code.copyWith(color: c.ink),
                onChanged: (_) {
                  if (_error != null) setState(() => _error = null);
                },
                onSubmitted: (_) => _submit(),
                decoration: InputDecoration(
                  hintText: l10n.machinesPairPasteHint,
                  errorText: _error,
                  errorMaxLines: 3,
                  suffixIcon: IconButton(
                    tooltip: l10n.machinesPairFromClipboard,
                    icon: const Icon(LucideIcons.clipboardPaste),
                    onPressed: _fromClipboard,
                  ),
                ),
              ),
              const SizedBox(height: Space.lg),
              ListenableBuilder(
                listenable: _text,
                builder: (context, _) => GalleyButton.solid(
                  label: l10n.machinesPairSubmit,
                  expand: true,
                  onPressed: _text.text.trim().isEmpty ? null : _submit,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
