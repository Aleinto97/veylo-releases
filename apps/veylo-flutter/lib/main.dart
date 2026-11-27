import 'dart:async';
import 'dart:ui' show PlatformDispatcher;
import 'package:flutter/services.dart';
import 'package:material_ui/material_ui.dart';
import 'package:media_kit/media_kit.dart';
import 'bridge/veylo_bridge.dart';
import 'screens/home_screen.dart';
import 'screens/library_screen.dart';
import 'screens/picks_screen.dart';
import 'screens/player_screen.dart';
import 'screens/search_screen.dart';
import 'screens/settings_screen.dart';
import 'screens/title_screen.dart';
import 'state/app_state.dart';
import 'theme/veylo_theme.dart';
import 'widgets/glass.dart';
import 'widgets/shortcuts_modal.dart';
import 'widgets/top_bar.dart';
import 'widgets/veylo_icons.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  _installErrorReporting();
  MediaKit.ensureInitialized();
  runApp(const VeyloApp());
}

/// Every uncaught Flutter/Dart error goes to the Rust bridge, which redacts it, keeps it in the
/// diagnostic report, and sends it to Sentry only when Opt-in diagnostics is on.
void _installErrorReporting() {
  final bridge = VeyloBridge.instance;
  final previous = FlutterError.onError;
  FlutterError.onError = (details) {
    previous?.call(details);
    // Network image misses (dead poster/logo URLs) are expected and handled by fallbacks.
    if (details.library == 'image resource service') return;
    unawaited(bridge.reportError(
      'flutter:${details.library ?? 'framework'}',
      details.exceptionAsString(),
      details.stack,
    ));
  };
  PlatformDispatcher.instance.onError = (error, stack) {
    unawaited(bridge.reportError('dart:uncaught', error, stack, fatal: true));
    return true;
  };
}

class VeyloApp extends StatefulWidget {
  const VeyloApp({super.key});

  @override
  State<VeyloApp> createState() => _VeyloAppState();
}

class _VeyloAppState extends State<VeyloApp> {
  final AppState _state = AppState();

  @override
  void initState() {
    super.initState();
    unawaited(_state.bootstrap());
  }

  ThemeData _buildTheme() {
    return ThemeData(
      brightness: Brightness.dark,
      fontFamily: VeyloText.uiFont,
      scaffoldBackgroundColor: Colors.transparent,
      canvasColor: Colors.transparent,
      splashFactory: NoSplash.splashFactory,
      splashColor: Colors.transparent,
      highlightColor: Colors.transparent,
      hoverColor: Colors.transparent,
      focusColor: Colors.transparent,
      colorScheme: const ColorScheme.dark(
        surface: VeyloColors.screen,
        primary: VeyloColors.bone,
        onPrimary: Color(0xFF000000),
        onSurface: VeyloColors.bone,
      ),
      tooltipTheme: TooltipThemeData(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
        decoration: BoxDecoration(
          color: const Color.fromRGBO(16, 17, 22, 0.94),
          borderRadius: BorderRadius.circular(10),
          border: Border.all(color: VeyloColors.edge),
          boxShadow: const [BoxShadow(color: Color.fromRGBO(0, 0, 0, 0.5), blurRadius: 14, offset: Offset(0, 4))],
        ),
        textStyle: VeyloText.ui(12.5, weight: 600, color: VeyloColors.bone),
      ),
      textSelectionTheme: const TextSelectionThemeData(
        cursorColor: VeyloColors.bone,
        selectionColor: Color.fromRGBO(242, 239, 234, 0.24),
        selectionHandleColor: VeyloColors.bone,
      ),
      scrollbarTheme: ScrollbarThemeData(
        thickness: const WidgetStatePropertyAll(5),
        radius: const Radius.circular(3),
        thumbColor: WidgetStateProperty.resolveWith((states) {
          if (states.contains(WidgetState.hovered) || states.contains(WidgetState.dragged)) {
            return const Color.fromRGBO(242, 239, 234, 0.34);
          }
          return const Color.fromRGBO(242, 239, 234, 0.14);
        }),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return AppStateScope(
      state: _state,
      child: MaterialApp(
        title: 'Veylo',
        debugShowCheckedModeBanner: false,
        theme: _buildTheme(),
        home: const Scaffold(
          backgroundColor: Colors.transparent,
          body: _VeyloShell(),
        ),
      ),
    );
  }
}

class _VeyloShell extends StatelessWidget {
  const _VeyloShell();

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.of(context);

    if (app.initError != null) {
      return ColoredBox(
        color: VeyloColors.screen,
        child: Center(
          child: GlassBox(
            borderRadius: 24,
            strong: true,
            padding: const EdgeInsets.all(28),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const VeyloIcon('Alert', size: 32, color: VeyloColors.amber),
                const SizedBox(height: 12),
                const Text('Failed to start Veylo backend', style: VeyloText.section),
                const SizedBox(height: 6),
                Text(app.initError!, style: VeyloText.credit),
              ],
            ),
          ),
        ),
      );
    }

    if (app.activePlayer != null) {
      return DefaultTextStyle(
        style: VeyloText.body,
        child: Stack(
          fit: StackFit.expand,
          children: [
            BackdropGroup(
              child: PlayerScreen(
                key: ValueKey('${app.activePlayer!.url}-${app.activePlayer!.title}'),
                session: app.activePlayer!,
              ),
            ),
            if (app.pendingPlayerTitle != null)
              _PendingOverlay(
                title: app.pendingPlayerTitle!,
                step: app.pendingPlayerStep,
                onCancel: app.cancelPendingPlay,
              ),
            if (app.toastMessage != null) _buildToast(app),
          ],
        ),
      );
    }

    final Key pageKey = app.openedTitle != null
        ? ValueKey('title-${app.openedTitle!.key}')
        : ValueKey('tab-${app.tab.name}');
    final Widget pageChild = app.openedTitle != null
        ? TitleScreen(key: pageKey, media: app.openedTitle!)
        : switch (app.tab) {
            VeyloTab.home => HomeScreen(key: pageKey),
            VeyloTab.picks => PicksScreen(key: pageKey),
            VeyloTab.library => LibraryScreen(key: pageKey),
            VeyloTab.settings => SettingsScreen(key: pageKey),
          };
    final Widget activePage = AnimatedSwitcher(
      duration: const Duration(milliseconds: 260),
      reverseDuration: const Duration(milliseconds: 200),
      switchInCurve: VeyloCurves.out,
      switchOutCurve: Curves.easeIn,
      transitionBuilder: (child, anim) {
        final isTitle = child.key.toString().contains('title-');
        if (isTitle) {
          return FadeTransition(
            opacity: anim,
            child: ScaleTransition(
              scale: Tween<double>(begin: 0.985, end: 1.0).animate(anim),
              child: AnimatedBuilder(
                animation: anim,
                child: child,
                builder: (_, c) => Transform.translate(
                  offset: Offset(0, (1.0 - anim.value) * 16.0),
                  child: c,
                ),
              ),
            ),
          );
        }
        final isEntering = child.key == pageKey;
        final sign = (isEntering ? 1.0 : -1.0) * app.tabDirection.toDouble();
        return FadeTransition(
          opacity: anim,
          child: AnimatedBuilder(
            animation: anim,
            child: child,
            builder: (_, c) => Transform.translate(
              offset: Offset((1.0 - anim.value) * 16.0 * sign, 0),
              child: c,
            ),
          ),
        );
      },
      child: pageChild,
    );

    return Focus(
      autofocus: true,
      onKeyEvent: (node, event) {
        if (event is! KeyDownEvent) return KeyEventResult.ignored;
        final ctrl = HardwareKeyboard.instance.isControlPressed || HardwareKeyboard.instance.isMetaPressed;
        final k = event.logicalKey;

        if (app.shortcutsOpen) {
          if (k == LogicalKeyboardKey.escape || event.character == '?') {
            app.setShortcutsOpen(false);
            return KeyEventResult.handled;
          }
          return KeyEventResult.ignored;
        }
        if (app.pendingPlayerTitle != null && k == LogicalKeyboardKey.escape) {
          app.cancelPendingPlay();
          return KeyEventResult.handled;
        }
        if (ctrl && k == LogicalKeyboardKey.keyK) {
          app.openSearch();
          return KeyEventResult.handled;
        }
        if (ctrl && k == LogicalKeyboardKey.digit1) { app.closeSearch(); app.setTab(VeyloTab.home); return KeyEventResult.handled; }
        if (ctrl && k == LogicalKeyboardKey.digit2) { app.closeSearch(); app.setTab(VeyloTab.picks); return KeyEventResult.handled; }
        if (ctrl && k == LogicalKeyboardKey.digit3) { app.closeSearch(); app.setTab(VeyloTab.library); return KeyEventResult.handled; }
        if (ctrl && k == LogicalKeyboardKey.digit4) { app.closeSearch(); app.setTab(VeyloTab.settings); return KeyEventResult.handled; }

        if (k == LogicalKeyboardKey.escape) {
          if (app.searchOpen) {
            app.closeSearch();
            return KeyEventResult.handled;
          }
          if (app.openedTitle != null) {
            app.closeTitle();
            return KeyEventResult.handled;
          }
        }
        final focusCtx = FocusManager.instance.primaryFocus?.context;
        final editingText = focusCtx != null &&
            (focusCtx.widget is EditableText || focusCtx.findAncestorWidgetOfExactType<EditableText>() != null);
        if (!editingText && !app.searchOpen && event.character == '?') {
          app.setShortcutsOpen(true);
          return KeyEventResult.handled;
        }
        if (!editingText && !app.searchOpen && event.character == '/') {
          app.openSearch();
          return KeyEventResult.handled;
        }
        return KeyEventResult.ignored;
      },
      child: ColoredBox(
        color: VeyloColors.screen,
        child: DefaultTextStyle(
          style: VeyloText.body,
          child: Stack(
            fit: StackFit.expand,
            children: [
              const AmbientBackground(),

              activePage,

              // Floating TopBar (owns its own BackdropGroup and 116px feathered frosted scroll smoke)
              const Positioned(top: 0, left: 0, right: 0, child: VeyloTopBar()),

              // Breathing PendingPlayer overlay (motion.css:83-85)
              if (app.pendingPlayerTitle != null)
                _PendingOverlay(
                  title: app.pendingPlayerTitle!,
                  step: app.pendingPlayerStep,
                  onCancel: app.cancelPendingPlay,
                ),

              // Search overlay
              if (app.searchOpen) BackdropGroup(child: const SearchOverlay()),

              // Shortcuts modal
              if (app.shortcutsOpen) BackdropGroup(child: const ShortcutsModal()),

              // Bottom glass toast (.toast)
              if (app.toastMessage != null) _buildToast(app),
            ],
          ),
        ),
      ),
    );
  }

  Widget _buildToast(AppState app) {
    return Positioned(
      left: 16,
      right: 16,
      bottom: 24,
      child: Center(
        child: BackdropGroup(
          child: GlassBox(
            borderRadius: 28,
            strong: true,
            padding: const EdgeInsets.fromLTRB(18, 6, 8, 6),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                VeyloIcon(
                  app.toastIsError ? 'Alert' : 'Sparkles',
                  size: 19,
                  color: app.toastIsError ? VeyloColors.amber : app.lampColor,
                  filled: !app.toastIsError,
                ),
                const SizedBox(width: 12),
                Flexible(
                  child: Text(
                    app.toastMessage!,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: VeyloText.ui(15, weight: 600),
                  ),
                ),
                const SizedBox(width: 8),
                VeyloOrb(icon: 'Close', size: 36, glass: false, onTap: app.dismissToast),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _PendingOverlay extends StatefulWidget {
  const _PendingOverlay({required this.title, required this.step, required this.onCancel});
  final String title;
  final String step;
  final VoidCallback onCancel;

  @override
  State<_PendingOverlay> createState() => _PendingOverlayState();
}

class _PendingOverlayState extends State<_PendingOverlay> with SingleTickerProviderStateMixin {
  late final AnimationController _breathe = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 3200),
  )..repeat(reverse: true);

  @override
  void dispose() {
    _breathe.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    return Positioned.fill(
      child: ValueListenableBuilder<Color>(
        valueListenable: app.lampColorNotifier,
        builder: (_, lamp, _) => ColoredBox(
          color: const Color(0xFF050506),
          child: Stack(
            fit: StackFit.expand,
            children: [
              FadeTransition(
                opacity: Tween<double>(begin: 0.55, end: 1.0).animate(CurvedAnimation(parent: _breathe, curve: Curves.easeInOut)),
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    gradient: RadialGradient(
                      center: const Alignment(0, -0.1),
                      radius: 0.75,
                      colors: [lamp.withValues(alpha: 0.20), const Color(0x00000000)],
                    ),
                  ),
                ),
              ),
              Positioned(
                top: 20,
                left: 20,
                child: Row(
                  children: [
                    VeyloOrb(icon: 'ArrowLeft', glass: false, onTap: widget.onCancel),
                    const SizedBox(width: 10),
                    Text(widget.title, style: VeyloText.display(22, weight: 700)),
                  ],
                ),
              ),
              Center(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(widget.step, style: VeyloText.bodyStrong),
                    const SizedBox(height: 18),
                    VeyloPill(label: 'Cancel', icon: 'Close', onTap: widget.onCancel),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
