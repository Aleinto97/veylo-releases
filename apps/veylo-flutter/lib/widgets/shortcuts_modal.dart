import 'dart:ui';
import 'package:flutter/widgets.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import 'glass.dart';
import 'veylo_icons.dart';

class ShortcutsModal extends StatelessWidget {
  const ShortcutsModal({super.key});

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.of(context);
    final rows = <(List<String>, String)>[];
    String ctx;

    if (app.pendingPlayerTitle != null) {
      ctx = 'Opening';
      rows.add((['Esc'], 'Cancel opening'));
    } else if (app.searchOpen) {
      ctx = 'Search';
      rows.addAll([
        (['Enter'], 'Search'),
        (['Esc'], 'Close search'),
      ]);
    } else if (app.openedTitle != null) {
      ctx = 'Title';
      rows.addAll([
        (['P'], 'Play or resume'),
        (['L'], 'Like'),
        (['D'], 'Not for me'),
        (['S'], 'Save'),
        (['W'], 'Watched'),
        (['O'], 'Choose source'),
        (['Esc'], 'Back'),
      ]);
    } else {
      ctx = app.tab.title;
      if (app.tab == VeyloTab.picks) {
        rows.addAll([
          (['←', '→'], 'Keep the left or right card'),
          (['S'], 'Skip: two other titles, nothing learned'),
          (['N'], 'Neither: steer away from both'),
        ]);
      }
    }

    if (app.pendingPlayerTitle == null) {
      rows.addAll([
        (['Ctrl', 'K'], 'Search'),
        (['Ctrl', '1–4'], 'Home / Picks / Library / Settings'),
      ]);
      if (!app.searchOpen) rows.add((['/'], 'Search'));
    }
    rows.add((['?'], 'Show or hide shortcuts'));

    return Positioned.fill(
      child: GestureDetector(
        onTap: () {
          app.setShortcutsOpen(false);
        },
        child: BackdropFilter.grouped(
          filter: ImageFilter.blur(sigmaX: 12, sigmaY: 12, tileMode: TileMode.clamp),
          child: Container(
            color: const Color.fromRGBO(0, 0, 0, 0.62),
            alignment: Alignment.center,
            padding: const EdgeInsets.all(24),
            child: GestureDetector(
              onTap: () {},
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 620, maxHeight: 580),
                child: GlassBox(
                  borderRadius: 28,
                  strong: true,
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Container(
                        padding: const EdgeInsets.fromLTRB(20, 14, 16, 14),
                        decoration: const BoxDecoration(
                          border: Border(bottom: BorderSide(color: VeyloColors.divider)),
                        ),
                        child: Row(
                          children: [
                            Container(
                              width: 34,
                              height: 34,
                              decoration: const BoxDecoration(
                                shape: BoxShape.circle,
                                color: Color.fromRGBO(255, 255, 255, 0.08),
                              ),
                              alignment: Alignment.center,
                              child: const VeyloIcon('Keyboard', size: 18, color: VeyloColors.bone),
                            ),
                            const SizedBox(width: 12),
                            Expanded(child: Text('$ctx shortcuts', style: VeyloText.groupName)),
                            VeyloOrb(
                              icon: 'Close',
                              size: 40,
                              onTap: () {
                                app.setShortcutsOpen(false);
                              },
                            ),
                          ],
                        ),
                      ),
                      Flexible(
                        child: SingleChildScrollView(
                          padding: const EdgeInsets.fromLTRB(22, 10, 22, 18),
                          child: Column(
                            children: [
                              for (final (keys, what) in rows)
                                SizedBox(
                                  height: 42,
                                  child: Row(
                                    children: [
                                      SizedBox(
                                        width: 126,
                                        child: Wrap(
                                          spacing: 5,
                                          children: [for (final k in keys) Keycap(k)],
                                        ),
                                      ),
                                      Expanded(
                                        child: Text(
                                          what,
                                          style: VeyloText.body.copyWith(color: VeyloColors.synopsis),
                                        ),
                                      ),
                                    ],
                                  ),
                                ),
                            ],
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
