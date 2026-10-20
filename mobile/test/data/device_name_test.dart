import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/device_name.dart';

void main() {
  test('用户起的名字可用时用它', () {
    expect(
      deviceNameFor(userAssigned: '小果的 iPhone', model: 'iPhone 12 Pro'),
      '小果的 iPhone',
    );
  });

  test('iOS 只给通用名时改用机型名', () {
    expect(
      deviceNameFor(userAssigned: 'iPhone', model: 'iPhone 12 Pro'),
      'iPhone 12 Pro',
    );
    expect(deviceNameFor(userAssigned: 'iPad', model: 'iPad Air'), 'iPad Air');
  });

  test('都拿不到时给一个能认出来的名字', () {
    expect(deviceNameFor(model: '  '), 'Smelt Mobile');
    expect(deviceNameFor(model: 'localhost'), 'Smelt Mobile');
    expect(deviceNameFor(model: 'Google Pixel 8'), 'Google Pixel 8');
  });
}
