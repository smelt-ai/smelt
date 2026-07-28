import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/main.dart';

void main() {
  testWidgets('shows pairing controls while disconnected', (tester) async {
    await tester.pumpWidget(const SmeltApp());

    expect(find.text('Not connected'), findsOneWidget);
    expect(find.text('Connect'), findsOneWidget);
    expect(find.text('Scan QR Code to Pair'), findsOneWidget);

    await tester.pumpWidget(const SizedBox.shrink());
  });
}
