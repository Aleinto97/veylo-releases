import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:veylo_flutter/theme/veylo_theme.dart';
import 'package:veylo_flutter/widgets/veylo_icons.dart';
import 'package:veylo_flutter/widgets/veylo_icons.g.dart';

void main() {
  testWidgets('Veylo theme and all exact icons.rs vectors render', (tester) async {
    expect(VeyloColors.parseHex('#e8b23a'), VeyloColors.amber);
    expect(kVeyloIcons.length, greaterThanOrEqualTo(60));
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Wrap(
          children: [
            for (final name in kVeyloIcons.keys)
              VeyloIcon(name, size: 20, filled: kVeyloFillableIcons.contains(name)),
          ],
        ),
      ),
    );
    expect(find.byType(VeyloIcon), findsNWidgets(kVeyloIcons.length));
  });
}
