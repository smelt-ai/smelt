import 'package:flutter/widgets.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:smelt_mobile/features/machines/machine_text.dart';

void main() {
  const color = Color(0xff123456);
  const size = 19.0;

  testWidgets('Linux 和 Windows 使用打包的品牌 SVG，按调用方尺寸和颜色显示', (tester) async {
    for (final (os, asset) in [
      (' Linux ', 'assets/machine-icons/linux.svg'),
      ('WINDOWS', 'assets/machine-icons/windows.svg'),
    ]) {
      // 同时验证资产已打包且能被实际 SVG 解码器解析。
      await tester.runAsync(() async {
        final picture = await vg.loadPicture(SvgAssetLoader(asset), null);
        expect(picture.size.width, greaterThan(0));
        expect(picture.size.height, greaterThan(0));
        picture.picture.dispose();
      });
      await tester.pumpWidget(
        Directionality(
          textDirection: TextDirection.ltr,
          child: MachineOsIcon(os: os, size: size, color: color),
        ),
      );
      final svg = tester.widget<SvgPicture>(find.byType(SvgPicture));
      expect((svg.bytesLoader as SvgAssetLoader).assetName, asset);
      expect(svg.width, size);
      expect(svg.height, size);
      expect(svg.colorFilter, const ColorFilter.mode(color, BlendMode.srcIn));
      expect(find.byType(Icon), findsNothing);
    }
  });

  testWidgets('Apple 和未知系统沿用原来的 Lucide 图标', (tester) async {
    for (final (os, icon) in [
      ('macos', LucideIcons.apple),
      (' Darwin ', LucideIcons.apple),
      (null, LucideIcons.laptopMinimal),
      ('', LucideIcons.laptopMinimal),
      ('freebsd', LucideIcons.laptopMinimal),
    ]) {
      await tester.pumpWidget(
        Directionality(
          textDirection: TextDirection.ltr,
          child: MachineOsIcon(os: os, size: size, color: color),
        ),
      );
      final widget = tester.widget<Icon>(find.byType(Icon));
      expect(widget.icon, icon);
      expect(widget.size, size);
      expect(widget.color, color);
      expect(find.byType(SvgPicture), findsNothing);
    }
  });
}
