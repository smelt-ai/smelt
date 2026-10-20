/// 第一层：原始值。**整个 App 里只有这个文件可以写颜色字面量**。
///
/// 按测得的值命名、不按用途命名：用途在 [GalleyColors]（第二层）里。改配色先改这里，
/// 再看语义槽要不要换指向。对比度在 `test/theme/contrast_test.dart` 里按 WCAG AA 验。
///
/// 视觉世界「出菜口」：拉丝钢灰底、白色搁板、热敏单据纸、墨黑、保温灯琥珀。
library;

import 'dart:ui';

abstract final class Primitives {
  // 钢（底）
  static const steel93 = Color(0xFFECEEF0);
  static const steel90 = Color(0xFFE3E6E9);
  static const steel82 = Color(0xFFCCD1D5);
  static const steel70 = Color(0xFFAEB4B9);
  static const steel97 = Color(0xFFF6F7F8);
  static const steel08 = Color(0xFF121417);
  static const steel11 = Color(0xFF1B1E22);
  static const steel14 = Color(0xFF22262B);
  static const steel18 = Color(0xFF2A2E33);
  static const steel24 = Color(0xFF363B41);
  static const steel30 = Color(0xFF454B52);

  static const white = Color(0xFFFFFFFF);

  // 纸（只给需要你动手的单据）
  static const paper99 = Color(0xFFFFFDF6);
  static const paper92 = Color(0xFFECE7DA);
  static const paperEdge84 = Color(0xFFD6D2C6);
  static const paperEdge70 = Color(0xFFB5AF9F);
  static const paper88 = Color(0xFFE6E1D3);
  static const paper84 = Color(0xFFDCD6C6);

  // 墨
  static const ink12 = Color(0xFF1E1E1E);
  static const ink40 = Color(0xFF596067);
  static const ink45 = Color(0xFF61686F);
  static const ink38onPaper = Color(0xFF5A5650);
  static const ink93 = Color(0xFFECEEF0);
  static const ink68 = Color(0xFFA2A9B0);
  static const ink60 = Color(0xFF8C939A);

  // 保温灯（琥珀）：只给「需要你」
  static const heat70 = Color(0xFFFF9F1C);
  static const heat74 = Color(0xFFFFA933);
  static const heatInk38 = Color(0xFF8F5300);
  static const heatInk76 = Color(0xFFFFB547);

  // 状态
  static const red45 = Color(0xFFC23B3B);
  static const red68 = Color(0xFFF07070);
  static const red95 = Color(0xFFFBE7E7);
  static const red20 = Color(0xFF3A1B1D);
  static const green32 = Color(0xFF1F7A4E);
  static const green52 = Color(0xFF2FA36B);
  static const green72 = Color(0xFF4CC48F);
  static const green95 = Color(0xFFE3F3EA);
  static const green18 = Color(0xFF16291F);
  static const blue45 = Color(0xFF2C67A8);
  static const blue74 = Color(0xFF86B8F0);

  // 灶台标签色：每台机器一条，深浅各一套，都对底色 ≥ 3:1（非文字图形）。
  static const stationLight = <Color>[
    Color(0xFF3B7DD8),
    Color(0xFF12857F),
    Color(0xFF7C5CD6),
    Color(0xFFC2468A),
    Color(0xFF6B7A1C),
    Color(0xFF4652C8),
    Color(0xFF1B7FB0),
    Color(0xFF93644A),
  ];
  static const stationDark = <Color>[
    Color(0xFF5B95E6),
    Color(0xFF2EB8B3),
    Color(0xFF9A80EC),
    Color(0xFFE06AAA),
    Color(0xFFA4B83D),
    Color(0xFF7480EE),
    Color(0xFF45B6EA),
    Color(0xFFC08D6E),
  ];

  // 阴影色（只用于真浮层与单据）
  static const shadow = Color(0x1F000000);
  static const shadowDeep = Color(0x52000000);
  static const scrim = Color(0x66000000);

  // 终端（深色固定，不跟运行时主题走）
  static const termBg = Color(0xFF101214);
  static const termFg = Color(0xFFE6E8EA);
}
