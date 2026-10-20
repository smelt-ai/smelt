import 'dart:io';

import 'package:device_info_plus/device_info_plus.dart';
import 'package:flutter/foundation.dart';

/// 配对时报给运行时的设备名，电脑端的设备列表里显示的就是它。
///
/// iOS 16 起，没有苹果单独批的 entitlement 读不到用户起的名字（系统只给通用的
/// 「iPhone」），所以 iOS 上实际用的是机型名，如「iPhone 12 Pro」。
Future<String> defaultDeviceName() async {
  final plugin = DeviceInfoPlugin();
  try {
    if (Platform.isIOS) {
      final info = await plugin.iosInfo;
      return deviceNameFor(userAssigned: info.name, model: info.modelName);
    }
    if (Platform.isAndroid) {
      final info = await plugin.androidInfo;
      return deviceNameFor(model: '${info.manufacturer} ${info.model}');
    }
  } catch (_) {
    // 读不到设备信息不该挡住配对，退回主机名。
  }
  return deviceNameFor(model: Platform.localHostname);
}

/// 系统不给真实名字时返回的通用名。
const _genericDeviceNames = {'iPhone', 'iPad', 'iPod touch'};

/// 用户起的名字可用就用它，否则用机型名。
@visibleForTesting
String deviceNameFor({String? userAssigned, required String model}) {
  final name = userAssigned?.trim() ?? '';
  if (name.isNotEmpty && !_genericDeviceNames.contains(name)) return name;
  final fallback = model.trim();
  if (fallback.isNotEmpty && fallback != 'localhost') return fallback;
  return 'Smelt Mobile';
}
