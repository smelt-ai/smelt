import 'dart:async';

import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import '../models/pairing_config.dart';
import '../theme/smelt_theme.dart';

class QrScannerPage extends StatefulWidget {
  const QrScannerPage({super.key});

  @override
  State<QrScannerPage> createState() => _QrScannerPageState();
}

class _QrScannerPageState extends State<QrScannerPage> {
  final MobileScannerController _controller = MobileScannerController(
    detectionSpeed: DetectionSpeed.noDuplicates,
    formats: const [BarcodeFormat.qrCode],
  );
  bool _finishing = false;
  String? _error;
  Timer? _errorTimer;

  /// 错误提示占的就是那条指引文案的位置。不自动收回的话，用户扫错一次之后
  /// 剩下的时间都在盯着一条过期的红字，反而不知道该怎么对准。
  void _showError(String message) {
    if (!mounted) return;
    setState(() => _error = message);
    _errorTimer?.cancel();
    _errorTimer = Timer(const Duration(seconds: 4), () {
      if (mounted) setState(() => _error = null);
    });
  }

  Future<void> _handleCapture(BarcodeCapture capture) async {
    if (_finishing) return;
    for (final barcode in capture.barcodes) {
      final raw = barcode.rawValue;
      if (raw == null || raw.isEmpty) continue;
      try {
        final pairing = PairingConfig.parse(raw);
        _finishing = true;
        _errorTimer?.cancel();
        await _controller.stop();
        if (mounted) Navigator.pop(context, pairing);
        return;
      } on FormatException catch (error) {
        _showError(error.message.toString());
      } catch (_) {
        _showError('Invalid Smelt pairing code');
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Scan Smelt Pairing Code'),
        actions: [
          ValueListenableBuilder(
            valueListenable: _controller,
            builder: (context, state, _) {
              if (state.torchState == TorchState.unavailable) {
                return const SizedBox.shrink();
              }
              return IconButton(
                tooltip: 'Toggle flashlight',
                onPressed: _controller.toggleTorch,
                icon: Icon(
                  state.torchState == TorchState.on
                      ? Icons.flash_on
                      : Icons.flash_off,
                ),
              );
            },
          ),
        ],
      ),
      body: Stack(
        fit: StackFit.expand,
        children: [
          MobileScanner(
            controller: _controller,
            onDetect: _handleCapture,
            errorBuilder: (context, error) => Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(
                  error.errorDetails?.message ?? 'Camera is unavailable',
                  textAlign: TextAlign.center,
                ),
              ),
            ),
          ),
          IgnorePointer(
            child: Center(
              child: Container(
                width: 250,
                height: 250,
                decoration: BoxDecoration(
                  border: Border.all(
                    color: Theme.of(context).colorScheme.primary,
                    width: 3,
                  ),
                  borderRadius: BorderRadius.circular(8),
                ),
              ),
            ),
          ),
          Positioned(
            left: 24,
            right: 24,
            bottom: 40,
            child: DecoratedBox(
              decoration: BoxDecoration(
                color: Colors.black87,
                borderRadius: BorderRadius.circular(8),
              ),
              child: Padding(
                padding: const EdgeInsets.all(12),
                child: Text(
                  _error ?? 'Point the camera at the QR code shown by Smelt',
                  textAlign: TextAlign.center,
                  style: TextStyle(
                    color: _error == null ? Colors.white : SmeltColors.dark.danger,
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }

  @override
  void dispose() {
    _errorTimer?.cancel();
    _controller.dispose();
    super.dispose();
  }
}
