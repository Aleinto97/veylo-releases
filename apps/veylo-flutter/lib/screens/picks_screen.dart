import 'dart:async';
import 'dart:math' as math;
import 'package:cached_network_image/cached_network_image.dart';
import 'package:material_ui/material_ui.dart' show InputDecoration, TextField;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/glass.dart';
import '../widgets/veylo_icons.dart';

enum _PicksSlot { mood, filters }

class PicksScreen extends StatefulWidget {
  const PicksScreen({super.key});

  @override
  State<PicksScreen> createState() => _PicksScreenState();
}

class _PicksScreenState extends State<PicksScreen> {
  Map<String, dynamic>? _data;
  bool _loading = true;
  bool _busy = false;
  _PicksSlot? _slot;
  bool _armNight = false;
  String? _keepingSide; // 'left' | 'right'
  int _voteSpin = 0;

  final TextEditingController _moodCtrl = TextEditingController();
  final TextEditingController _fromCtrl = TextEditingController();
  final TextEditingController _toCtrl = TextEditingController();
  final FocusNode _keyFocus = FocusNode();
  final FocusNode _moodFocus = FocusNode();
  final FocusNode _fromFocus = FocusNode();
  final FocusNode _toFocus = FocusNode();

  @override
  void initState() {
    super.initState();
    _moodFocus.addListener(_onFieldFocus);
    _fromFocus.addListener(_onFieldFocus);
    _toFocus.addListener(_onFieldFocus);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      unawaited(_load());
      _keyFocus.requestFocus();
    });
  }

  void _onFieldFocus() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _moodFocus.removeListener(_onFieldFocus);
    _fromFocus.removeListener(_onFieldFocus);
    _toFocus.removeListener(_onFieldFocus);
    _moodCtrl.dispose();
    _fromCtrl.dispose();
    _toCtrl.dispose();
    _keyFocus.dispose();
    _moodFocus.dispose();
    _fromFocus.dispose();
    _toFocus.dispose();
    super.dispose();
  }

  Future<void> _load({bool force = false}) async {
    final app = AppStateScope.read(context);
    setState(() => _loading = true);
    try {
      final res = await app.bridge.picksLoad(savedOnly: app.picksSavedOnly, force: force);
      if (!mounted) return;
      _applyData(res);
    } catch (e) {
      if (mounted) {
        setState(() => _loading = false);
        app.showToast('Picks error: $e', isError: true);
      }
    }
  }

  void _applyData(Map<String, dynamic> res) {
    final nextFrom = res['from']?.toString() ?? '';
    final nextTo = res['to']?.toString() ?? '';
    setState(() {
      _data = res;
      _loading = false;
      _busy = false;
      _keepingSide = null;
      if (!_fromFocus.hasFocus) _fromCtrl.text = nextFrom;
      if (!_toFocus.hasFocus) _toCtrl.text = nextTo;
    });
    _syncLamp(res);
  }

  void _syncLamp(Map<String, dynamic> res) {
    final app = AppStateScope.read(context);
    final left = res['left'] as Map<String, dynamic>?;
    final right = res['right'] as Map<String, dynamic>?;
    final card = left ?? right;
    if (card == null) {
      unawaited(app.setLampSource(''));
      return;
    }
    final m = MediaItem.fromJson(card['media'] as Map<String, dynamic>);
    final art = app.cachedArtwork(m.key);
    final bg = (art?.backdrop?.isNotEmpty == true) ? art!.backdrop! : m.backdrop;
    if (bg.isNotEmpty) unawaited(app.setLampSource(bg));
    if (left != null) unawaited(app.fetchArtwork(MediaItem.fromJson(left['media'] as Map<String, dynamic>)));
    if (right != null) unawaited(app.fetchArtwork(MediaItem.fromJson(right['media'] as Map<String, dynamic>)));
  }

  Future<void> _choose(bool left) async {
    if (_busy || _data == null) return;
    setState(() {
      _busy = true;
      _keepingSide = left ? 'left' : 'right';
      _voteSpin++;
    });
    final bridge = AppStateScope.read(context).bridge;
    await Future.delayed(const Duration(milliseconds: 165));
    if (!mounted) return;
    try {
      final res = await bridge.picksChoose(left: left);
      if (mounted) _applyData(res);
    } catch (_) {
      if (mounted) {
        setState(() {
          _busy = false;
          _keepingSide = null;
        });
      }
    }
  }

  Future<void> _skip(bool neither) async {
    if (_busy || _data == null) return;
    setState(() {
      _busy = true;
      _voteSpin++;
    });
    try {
      final res = await AppStateScope.read(context).bridge.picksSkip(neither: neither);
      if (mounted) _applyData(res);
    } catch (_) {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _toggleMood(String label, {String? phrase, bool clear = false}) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      final res = await AppStateScope.read(context).bridge.picksMood(label: label, phrase: phrase, clear: clear);
      if (mounted) _applyData(res);
    } catch (_) {
      if (mounted) setState(() => _busy = false);
    }
  }

  String? _yearValidationError(String from, String to) {
    final f = from.trim();
    final t = to.trim();
    if (f.isNotEmpty && !RegExp(r'^\d{4}$').hasMatch(f)) return 'Enter 4 digits';
    if (t.isNotEmpty && !RegExp(r'^\d{4}$').hasMatch(t)) return 'Enter 4 digits';
    if (f.isNotEmpty && t.isNotEmpty) {
      final fi = int.parse(f);
      final ti = int.parse(t);
      if (fi > ti) return 'From must be ≤ To';
    }
    return null;
  }

  void _maybeApplyYears() {
    final f = _fromCtrl.text.trim();
    final t = _toCtrl.text.trim();
    if (_yearValidationError(f, t) != null) return;
    final curFrom = _data?['from']?.toString() ?? '';
    final curTo = _data?['to']?.toString() ?? '';
    if (f != curFrom || t != curTo) {
      unawaited(_updateFilters(from: f, to: t));
    }
  }

  Future<void> _updateFilters({
    String? kind,
    bool setKind = false,
    String? style,
    List<String>? genres,
    String? from,
    String? to,
    bool? savedOnly,
  }) async {
    final d = _data;
    if (d == null) return;
    final app = AppStateScope.read(context);
    final nextSaved = savedOnly ?? (d['saved_only'] as bool? ?? false);
    app.picksSavedOnly = nextSaved;
    setState(() => _busy = true);
    try {
      final res = await app.bridge.picksFilters(
        kind: setKind ? kind : d['kind'] as String?,
        style: style ?? (d['style'] as String? ?? 'both'),
        genres: genres ?? (d['genres'] as List<dynamic>? ?? []).map((e) => e.toString()).toList(),
        from: from ?? (d['from'] as String? ?? ''),
        to: to ?? (d['to'] as String? ?? ''),
        savedOnly: nextSaved,
      );
      if (mounted) _applyData(res);
    } catch (_) {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _newNight() async {
    setState(() {
      _armNight = false;
      _slot = null;
      _busy = true;
    });
    try {
      final res = await AppStateScope.read(context).bridge.picksNewNight();
      if (mounted) _applyData(res);
    } catch (_) {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final size = MediaQuery.sizeOf(context);
    final gutter = VeyloLayout.gutter(size.width);
    final d = _data;

    final votes = (d?['votes'] as num?)?.toInt() ?? 0;
    final lamps = (d?['lamps'] as num?)?.toInt() ?? 0;
    final leaning = (d?['leaning'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
    final moods = (d?['moods'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
    final shortlist = (d?['shortlist'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
    final savedOnly = d?['saved_only'] as bool? ?? app.picksSavedOnly;
    final aiFailed = d?['ai_failed'] as bool? ?? app.aiFailed;
    final leftCard = d?['left'] as Map<String, dynamic>?;
    final rightCard = d?['right'] as Map<String, dynamic>?;
    final nightHasState = votes > 0 || moods.isNotEmpty || shortlist.isNotEmpty;

    final kindVal = d?['kind'] as String?;
    final styleVal = d?['style'] as String? ?? 'both';
    final chosenGenres = (d?['genres'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
    final fromVal = d?['from']?.toString() ?? '';
    final toVal = d?['to']?.toString() ?? '';
    final yearPart = (fromVal.isNotEmpty || toVal.isNotEmpty)
        ? '${fromVal.isEmpty ? '…' : fromVal}–${toVal.isEmpty ? 'now' : toVal}'
        : null;
    final filterParts = <String>[
      if (kindVal == 'movie') 'Films' else if (kindVal == 'series') 'Series',
      if (styleVal == 'live') 'Live' else if (styleVal == 'animation') 'Animation',
      ...chosenGenres.take(2),
      ?yearPart,
    ];
    final filterLabel = filterParts.isEmpty ? 'Filters' : filterParts.join(' · ');

    return Focus(
      focusNode: _keyFocus,
      onKeyEvent: (node, event) {
        if (event is! KeyDownEvent || _slot != null) return KeyEventResult.ignored;
        if (event.logicalKey == LogicalKeyboardKey.arrowLeft) {
          unawaited(_choose(true));
          return KeyEventResult.handled;
        }
        if (event.logicalKey == LogicalKeyboardKey.arrowRight) {
          unawaited(_choose(false));
          return KeyEventResult.handled;
        }
        if (event.logicalKey == LogicalKeyboardKey.keyS) {
          unawaited(_skip(false));
          return KeyEventResult.handled;
        }
        if (event.logicalKey == LogicalKeyboardKey.keyN) {
          unawaited(_skip(true));
          return KeyEventResult.handled;
        }
        return KeyEventResult.ignored;
      },
      child: Stack(
        fit: StackFit.expand,
        children: [
          Padding(
            padding: EdgeInsets.fromLTRB(gutter, 88, gutter, 18),
            child: Column(
              children: [
                // duel-bar
                ValueListenableBuilder<Color>(
                  valueListenable: app.lampColorNotifier,
                  builder: (_, lamp, _) => Row(
                    crossAxisAlignment: CrossAxisAlignment.end,
                    children: [
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Row(
                              children: [
                                Text('Tonight', style: VeyloText.display(34, weight: 800, height: 1.02)),
                                if (votes > 0) ...[
                                  const SizedBox(width: 14),
                                  Text(
                                    '$votes ${votes == 1 ? 'vote' : 'votes'}',
                                    style: VeyloText.ui(14.5, weight: 600, color: VeyloColors.dust, tabular: true),
                                  ),
                                ],
                                const SizedBox(width: 14),
                                Row(
                                  children: [
                                    for (int i = 0; i < 5; i++) ...[
                                      if (i > 0) const SizedBox(width: 4),
                                      AnimatedContainer(
                                        duration: const Duration(milliseconds: 450),
                                        curve: VeyloCurves.out,
                                        width: 18,
                                        height: 6,
                                        decoration: BoxDecoration(
                                          borderRadius: BorderRadius.circular(3),
                                          color: i < lamps ? lamp : const Color.fromRGBO(255, 255, 255, 0.14),
                                          boxShadow: i < lamps
                                              ? [BoxShadow(color: lamp.withValues(alpha: 0.6), blurRadius: 10)]
                                              : null,
                                        ),
                                      ),
                                    ],
                                  ],
                                ),
                              ],
                            ),
                            const SizedBox(height: 8),
                            Wrap(
                              spacing: 8,
                              runSpacing: 6,
                              crossAxisAlignment: WrapCrossAlignment.center,
                              children: [
                                if (leaning.isNotEmpty) ...[
                                  Text('Leaning', style: VeyloText.ui(14.5, weight: 500, color: VeyloColors.dust)),
                                  for (final l in leaning)
                                    Container(
                                      height: 30,
                                      padding: const EdgeInsets.symmetric(horizontal: 12),
                                      alignment: Alignment.center,
                                      decoration: BoxDecoration(
                                        color: VeyloColors.mixLamp(lamp, const Color.fromRGBO(255, 255, 255, 0.08), 0.18),
                                        borderRadius: BorderRadius.circular(15),
                                        border: Border.all(color: const Color.fromRGBO(255, 255, 255, 0.14)),
                                      ),
                                      child: Text(l, style: VeyloText.ui(13, weight: 600, color: VeyloColors.bone)),
                                    ),
                                ] else
                                  Text(
                                    votes == 0 ? 'Keep the one you\'d rather watch. Picks learns as you go.' : 'Getting to know tonight…',
                                    style: VeyloText.ui(14.5, weight: 500, color: VeyloColors.dust),
                                  ),
                                for (final mood in moods)
                                  GestureDetector(
                                    onTap: () => _toggleMood(mood),
                                    child: MouseRegion(
                                      cursor: SystemMouseCursors.click,
                                      child: Container(
                                        height: 30,
                                        padding: const EdgeInsets.symmetric(horizontal: 12),
                                        decoration: BoxDecoration(
                                          color: VeyloColors.bone,
                                          borderRadius: BorderRadius.circular(15),
                                        ),
                                        child: Row(
                                          mainAxisSize: MainAxisSize.min,
                                          children: [
                                            const VeyloIcon('Sparkles', size: 14, color: Color(0xFF000000), filled: true),
                                            const SizedBox(width: 6),
                                            Text(mood, style: VeyloText.ui(13, weight: 700, color: const Color(0xFF000000))),
                                            const SizedBox(width: 6),
                                            const VeyloIcon('Close', size: 14, color: Color(0xFF000000)),
                                          ],
                                        ),
                                      ),
                                    ),
                                  ),
                              ],
                            ),
                          ],
                        ),
                      ),
                      const SizedBox(width: 16),
                      Wrap(
                        spacing: 10,
                        crossAxisAlignment: WrapCrossAlignment.center,
                        children: [
                          if (savedOnly)
                            VeyloPill(
                              label: 'Saved only',
                              icon: 'Bookmark',
                              active: true,
                              onTap: () => _updateFilters(savedOnly: false),
                            ),
                          VeyloPill(
                            label: 'Mood',
                            icon: 'Sparkles',
                            active: _slot == _PicksSlot.mood || moods.isNotEmpty,
                            disabled: aiFailed,
                            onTap: () => setState(() => _slot = _slot == _PicksSlot.mood ? null : _PicksSlot.mood),
                          ),
                          if (!savedOnly)
                            VeyloPill(
                              label: filterLabel,
                              icon: 'Sliders',
                              active: _slot == _PicksSlot.filters || filterParts.isNotEmpty,
                              onTap: () => setState(() => _slot = _slot == _PicksSlot.filters ? null : _PicksSlot.filters),
                            ),
                          if (_armNight) ...[
                            VeyloPill(label: 'Reset tonight', icon: 'Moon', confirmRed: true, onTap: _newNight),
                            VeyloOrb(icon: 'Close', size: 42, onTap: () => setState(() => _armNight = false)),
                          ] else
                            VeyloPill(
                              label: 'New night',
                              icon: 'Moon',
                              onTap: () {
                                if (!nightHasState) {
                                  unawaited(_newNight());
                                } else {
                                  setState(() {
                                    _slot = null;
                                    _armNight = true;
                                  });
                                }
                              },
                            ),
                        ],
                      ),
                    ],
                  ),
                ),

                const SizedBox(height: 16),

                // Duel cards (.duel-cards)
                Expanded(
                  child: _loading
                      ? Row(
                          children: [
                            const Expanded(child: SkeletonBox(borderRadius: 28)),
                            const SizedBox(width: 16),
                            _vsMedallion(),
                            const SizedBox(width: 16),
                            const Expanded(child: SkeletonBox(borderRadius: 28)),
                          ],
                        )
                      : (leftCard == null && rightCard == null)
                          ? Center(
                              child: GlassBox(
                                borderRadius: 24,
                                padding: const EdgeInsets.symmetric(horizontal: 32, vertical: 28),
                                child: Column(
                                  mainAxisSize: MainAxisSize.min,
                                  children: [
                                    const VeyloIcon('Search', size: 30, color: VeyloColors.dust),
                                    const SizedBox(height: 10),
                                    const Text('Nothing matches these filters', style: VeyloText.body),
                                    const SizedBox(height: 14),
                                    VeyloPill(
                                      label: 'Clear filters',
                                      icon: 'Undo',
                                      onTap: () => _updateFilters(
                                        kind: null,
                                        setKind: true,
                                        style: 'both',
                                        genres: [],
                                        from: '',
                                        to: '',
                                        savedOnly: false,
                                      ),
                                    ),
                                  ],
                                ),
                              ),
                            )
                          : Row(
                              crossAxisAlignment: CrossAxisAlignment.stretch,
                              children: [
                                Expanded(
                                  child: leftCard != null
                                      ? _DuelCard(
                                          key: ValueKey('L-${leftCard['media']['key']}'),
                                          data: leftCard,
                                          isLeft: true,
                                          state: _keepingSide == 'left' ? 1 : (_keepingSide == 'right' ? -1 : 0),
                                          onKeep: () => _choose(true),
                                          onRefresh: () => _load(force: false),
                                        )
                                      : const GlassBox(
                                          borderRadius: 28,
                                          child: Center(child: Text('No other match for these filters.', style: VeyloText.meta)),
                                        ),
                                ),
                                const SizedBox(width: 16),
                                _vsMedallion(),
                                const SizedBox(width: 16),
                                Expanded(
                                  child: rightCard != null
                                      ? _DuelCard(
                                          key: ValueKey('R-${rightCard['media']['key']}'),
                                          data: rightCard,
                                          isLeft: false,
                                          state: _keepingSide == 'right' ? 1 : (_keepingSide == 'left' ? -1 : 0),
                                          onKeep: () => _choose(false),
                                          onRefresh: () => _load(force: false),
                                        )
                                      : const GlassBox(
                                          borderRadius: 28,
                                          child: Center(child: Text('No other match for these filters.', style: VeyloText.meta)),
                                        ),
                                ),
                              ],
                            ),
                ),

                const SizedBox(height: 14),

                // Footer: Skip / Neither + Shortlist (.duel-foot)
                SizedBox(
                  height: 70,
                  child: Row(
                    children: [
                      VeyloPill(
                        label: 'Skip',
                        icon: 'Next',
                        disabled: _loading || _busy,
                        trailing: const Keycap('S', small: true),
                        onTap: () => _skip(false),
                      ),
                      const SizedBox(width: 10),
                      VeyloPill(
                        label: 'Neither',
                        icon: 'Close',
                        disabled: _loading || _busy,
                        trailing: const Keycap('N', small: true),
                        onTap: () => _skip(true),
                      ),
                      const SizedBox(width: 20),
                      Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          const VeyloIcon('Trophy', size: 18, color: VeyloColors.amber),
                          const SizedBox(width: 7),
                          Text('Shortlist', style: VeyloText.ui(15, weight: 700, color: VeyloColors.dust)),
                        ],
                      ),
                      const SizedBox(width: 12),
                      Expanded(
                        child: shortlist.isEmpty
                            ? Text('Titles you keep land here, best first.', style: VeyloText.ui(14, weight: 500, color: VeyloColors.dust))
                            : ListView.separated(
                                scrollDirection: Axis.horizontal,
                                itemCount: shortlist.length,
                                separatorBuilder: (_, _) => const SizedBox(width: 10),
                                itemBuilder: (_, i) => _ShortItem(
                                  entry: shortlist[i],
                                  onRemove: () async {
                                    final m = MediaItem.fromJson(shortlist[i]['media'] as Map<String, dynamic>);
                                    final res = await app.bridge.picksUnstar(m.key);
                                    if (mounted) _applyData(res);
                                  },
                                ),
                              ),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),

          // Popover backdrop & popovers
          if (_slot != null)
            Positioned.fill(
              child: GestureDetector(
                onTap: () {
                  _maybeApplyYears();
                  setState(() => _slot = null);
                },
                child: const ColoredBox(color: Color(0x01000000)),
              ),
            ),
          if (_slot == _PicksSlot.mood)
            Positioned(
              right: gutter,
              top: 158,
              child: LiftIn(offsetY: -6, duration: const Duration(milliseconds: 180), child: _buildMoodPopover(app, moods)),
            ),
          if (_slot == _PicksSlot.filters)
            Positioned(
              right: gutter,
              top: 158,
              child: LiftIn(offsetY: -6, duration: const Duration(milliseconds: 180), child: _buildFiltersPopover(app)),
            ),
        ],
      ),
    );
  }

  /// VS medallion with CSS `versus-spin` 180° flip on vote.
  Widget _vsMedallion() {
    return Center(
      child: TweenAnimationBuilder<double>(
        tween: Tween(begin: 0, end: _voteSpin.toDouble()),
        duration: const Duration(milliseconds: 520),
        curve: VeyloCurves.out,
        builder: (_, turns, child) => Transform.rotate(
          angle: turns * math.pi,
          child: child,
        ),
        child: GlassBox(
          borderRadius: 26,
          strong: true,
          child: SizedBox(
            width: 52,
            height: 52,
            child: Center(
              child: Text(
                'VS',
                style: VeyloText.display(16, weight: 800, letterSpacing: 0.7),
              ),
            ),
          ),
        ),
      ),
    );
  }

  Widget _buildMoodPopover(AppState app, List<String> activeMoods) {
    final available = (_data?['available_moods'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
    return ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 580, maxHeight: 480),
      child: GlassBox(
        borderRadius: 22,
        strong: true,
        padding: const EdgeInsets.all(18),
        child: SingleChildScrollView(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                'Pick a mood or describe one. Picks leans toward it all night; tap again to drop it.',
                style: VeyloText.ui(14, weight: 500, color: VeyloColors.dust),
              ),
              const SizedBox(height: 14),
              Wrap(
                spacing: 6,
                runSpacing: 6,
                children: [
                  for (final m in available)
                    _OptionChip(
                      label: m['label']?.toString() ?? '',
                      active: activeMoods.contains(m['label']?.toString()),
                      onTap: () => _toggleMood(m['label'].toString(), phrase: m['phrase']?.toString()),
                    ),
                ],
              ),
              const SizedBox(height: 14),
              Row(
                children: [
                  Expanded(
                    child: ValueListenableBuilder<Color>(
                      valueListenable: app.lampColorNotifier,
                      builder: (_, lamp, _) => Container(
                        height: 46,
                        padding: const EdgeInsets.symmetric(horizontal: 16),
                        alignment: Alignment.centerLeft,
                        decoration: BoxDecoration(
                          color: const Color.fromRGBO(255, 255, 255, 0.08),
                          borderRadius: BorderRadius.circular(23),
                          border: Border.all(
                            color: _moodFocus.hasFocus ? lamp : const Color.fromRGBO(255, 255, 255, 0.12),
                            width: _moodFocus.hasFocus ? 1.5 : 1,
                          ),
                        ),
                        child: TextField(
                          controller: _moodCtrl,
                          focusNode: _moodFocus,
                          style: VeyloText.ui(14.5, weight: 500),
                          cursorColor: lamp,
                          decoration: InputDecoration.collapsed(
                            hintText: 'Describe a mood: “slow-burn heist in the snow”',
                            hintStyle: VeyloText.ui(14, weight: 500, color: VeyloColors.dust),
                          ),
                          onChanged: (_) => setState(() {}),
                          onSubmitted: (v) {
                            if (v.trim().isNotEmpty) {
                              final t = v.trim();
                              _moodCtrl.clear();
                              unawaited(_toggleMood(t, phrase: t));
                            }
                          },
                        ),
                      ),
                    ),
                  ),
                  const SizedBox(width: 10),
                  VeyloPill(
                    label: 'Add',
                    icon: 'Sparkles',
                    accent: true,
                    disabled: _moodCtrl.text.trim().isEmpty,
                    onTap: () {
                      final t = _moodCtrl.text.trim();
                      if (t.isNotEmpty) {
                        _moodCtrl.clear();
                        unawaited(_toggleMood(t, phrase: t));
                      }
                    },
                  ),
                ],
              ),
              const SizedBox(height: 14),
              Row(
                mainAxisAlignment: MainAxisAlignment.end,
                children: [
                  if (activeMoods.isNotEmpty) ...[
                    VeyloPill(label: 'Clear moods', icon: 'Undo', onTap: () => _toggleMood('', clear: true)),
                    const SizedBox(width: 10),
                  ],
                  VeyloPill(label: 'Done', icon: 'Check', accent: true, onTap: () => setState(() => _slot = null)),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _buildFiltersPopover(AppState app) {
    final d = _data!;
    final kind = d['kind'] as String?;
    final style = d['style'] as String? ?? 'both';
    final chosenGenres = (d['genres'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
    final allGenres = (d['available_genres'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
    final hideWatched = d['hide_watched'] as bool? ?? true;
    final yearErr = _yearValidationError(_fromCtrl.text, _toCtrl.text);

    return ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 600, maxHeight: 540),
      child: GlassBox(
        borderRadius: 22,
        strong: true,
        padding: const EdgeInsets.all(18),
        child: SingleChildScrollView(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Wrap(
                spacing: 10,
                runSpacing: 10,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  VeyloSegmented<String?>(
                    value: kind,
                    options: const [
                      (null, '', 'Grid'),
                      ('movie', '', 'Film'),
                      ('series', '', 'Tv'),
                    ],
                    onChanged: (v) => _updateFilters(kind: v, setKind: true),
                  ),
                  VeyloSegmented<String>(
                    value: style,
                    options: const [
                      ('both', '', 'Layers'),
                      ('live', '', 'Camera'),
                      ('animation', '', 'Blossom'),
                    ],
                    onChanged: (v) => _updateFilters(style: v),
                  ),
                  VeyloPill(
                    label: 'Hide watched',
                    icon: 'EyeOff',
                    active: hideWatched,
                    small: true,
                    onTap: () async {
                      await app.bridge.settingsUpdate({'picks_hide_watched': !hideWatched});
                      await _updateFilters();
                    },
                  ),
                ],
              ),
              const SizedBox(height: 14),
              // Years filter row (From – To)
              ValueListenableBuilder<Color>(
                valueListenable: app.lampColorNotifier,
                builder: (_, lamp, _) => Wrap(
                  spacing: 10,
                  runSpacing: 8,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    Text('Years', style: VeyloText.ui(13.5, weight: 600, color: VeyloColors.dust)),
                    _yearInput(_fromCtrl, _fromFocus, 'From', lamp),
                    Text('–', style: VeyloText.ui(15, weight: 600, color: VeyloColors.dust)),
                    _yearInput(_toCtrl, _toFocus, 'To', lamp),
                    if (yearErr != null)
                      Text(yearErr, style: VeyloText.ui(12.5, weight: 600, color: VeyloColors.amber))
                    else if (_fromCtrl.text.isNotEmpty || _toCtrl.text.isNotEmpty)
                      VeyloOrb(
                        icon: 'Close',
                        size: 32,
                        iconSize: 14,
                        tooltip: 'Clear years',
                        onTap: () {
                          _fromCtrl.clear();
                          _toCtrl.clear();
                          unawaited(_updateFilters(from: '', to: ''));
                        },
                      ),
                  ],
                ),
              ),
              const SizedBox(height: 14),
              Wrap(
                spacing: 6,
                runSpacing: 6,
                children: [
                  _OptionChip(
                    label: 'Any genre',
                    active: chosenGenres.isEmpty,
                    onTap: () => _updateFilters(genres: []),
                  ),
                  for (final g in allGenres)
                    _OptionChip(
                      label: g,
                      active: chosenGenres.contains(g),
                      onTap: () {
                        final next = List<String>.from(chosenGenres);
                        if (next.contains(g)) {
                          next.remove(g);
                        } else {
                          next.add(g);
                        }
                        unawaited(_updateFilters(genres: next));
                      },
                    ),
                ],
              ),
              const SizedBox(height: 14),
              Row(
                mainAxisAlignment: MainAxisAlignment.end,
                children: [
                  VeyloPill(
                    label: 'Clear filters',
                    icon: 'Undo',
                    onTap: () {
                      _fromCtrl.clear();
                      _toCtrl.clear();
                      unawaited(_updateFilters(kind: null, setKind: true, style: 'both', genres: [], from: '', to: ''));
                    },
                  ),
                  const SizedBox(width: 10),
                  VeyloPill(
                    label: 'Done',
                    icon: 'Check',
                    accent: true,
                    onTap: () {
                      _maybeApplyYears();
                      setState(() => _slot = null);
                    },
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _yearInput(TextEditingController ctrl, FocusNode focus, String hint, Color lamp) {
    return Container(
      width: 82,
      height: 36,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: const Color.fromRGBO(255, 255, 255, 0.08),
        borderRadius: BorderRadius.circular(18),
        border: Border.all(
          color: focus.hasFocus ? lamp : const Color.fromRGBO(255, 255, 255, 0.12),
          width: focus.hasFocus ? 1.5 : 1,
        ),
      ),
      child: TextField(
        controller: ctrl,
        focusNode: focus,
        textAlign: TextAlign.center,
        keyboardType: TextInputType.number,
        inputFormatters: [FilteringTextInputFormatter.digitsOnly, LengthLimitingTextInputFormatter(4)],
        style: VeyloText.ui(14, weight: 600, tabular: true),
        cursorColor: lamp,
        decoration: InputDecoration.collapsed(
          hintText: hint,
          hintStyle: VeyloText.ui(13.5, weight: 500, color: VeyloColors.dust),
        ),
        onChanged: (_) {
          setState(() {});
          if (_fromCtrl.text.length == 4 || _fromCtrl.text.isEmpty) {
            if (_toCtrl.text.length == 4 || _toCtrl.text.isEmpty) {
              _maybeApplyYears();
            }
          }
        },
        onSubmitted: (_) => _maybeApplyYears(),
      ),
    );
  }
}

class _OptionChip extends StatefulWidget {
  const _OptionChip({required this.label, required this.active, required this.onTap});
  final String label;
  final bool active;
  final VoidCallback onTap;

  @override
  State<_OptionChip> createState() => _OptionChipState();
}

class _OptionChipState extends State<_OptionChip> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 140),
          height: 40,
          padding: const EdgeInsets.symmetric(horizontal: 15),
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: widget.active
                ? VeyloColors.bone
                : (_hovered ? const Color.fromRGBO(255, 255, 255, 0.12) : const Color.fromRGBO(255, 255, 255, 0.06)),
            borderRadius: BorderRadius.circular(20),
          ),
          child: Text(
            widget.label,
            style: VeyloText.ui(
              14,
              weight: 600,
              color: widget.active ? const Color(0xFF000000) : (_hovered ? VeyloColors.bone : VeyloColors.dust),
            ),
          ),
        ),
      ),
    );
  }
}

class _DuelCard extends StatefulWidget {
  const _DuelCard({
    super.key,
    required this.data,
    required this.isLeft,
    required this.state,
    required this.onKeep,
    required this.onRefresh,
  });

  final Map<String, dynamic> data;
  final bool isLeft;
  final int state; // 1 = kept, -1 = fading out, 0 = idle
  final VoidCallback onKeep;
  final VoidCallback onRefresh;

  @override
  State<_DuelCard> createState() => _DuelCardState();
}

class _DuelCardState extends State<_DuelCard> {
  final ValueNotifier<Offset> _tilt = ValueNotifier<Offset>(Offset.zero);
  final ValueNotifier<double> _dragX = ValueNotifier<double>(0.0);
  bool _hovered = false;
  bool _dragging = false;
  late bool _liked;
  late bool _saved;
  Color? _cardLamp;

  @override
  void initState() {
    super.initState();
    _liked = widget.data['liked'] as bool? ?? false;
    _saved = widget.data['saved'] as bool? ?? false;
    WidgetsBinding.instance.addPostFrameCallback((_) => _loadCardExtras());
  }

  @override
  void dispose() {
    _tilt.dispose();
    _dragX.dispose();
    super.dispose();
  }

  void _updateTilt(BuildContext ctx, Offset localPos) {
    final box = ctx.findRenderObject() as RenderBox?;
    if (box == null || box.size.width <= 0 || box.size.height <= 0) return;
    final nx = ((localPos.dx / box.size.width) * 2.0 - 1.0).clamp(-1.0, 1.0);
    final ny = ((localPos.dy / box.size.height) * 2.0 - 1.0).clamp(-1.0, 1.0);
    _tilt.value = Offset(nx, ny);
  }

  Future<void> _loadCardExtras() async {
    final app = AppStateScope.read(context);
    final m = MediaItem.fromJson(widget.data['media'] as Map<String, dynamic>);
    final art = await app.fetchArtwork(m);
    final bg = (art?.backdrop?.isNotEmpty == true) ? art!.backdrop! : m.backdrop;
    if (bg.isNotEmpty) {
      final c = await app.fetchAccent(bg);
      if (mounted && c != null) setState(() => _cardLamp = c);
    }
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final m = MediaItem.fromJson(widget.data['media'] as Map<String, dynamic>);
    final matching = (widget.data['matching'] as num?)?.toInt();
    final wins = (widget.data['wins'] as num?)?.toInt() ?? 0;
    final why = widget.data['why'] as String?;
    final feels = (widget.data['feels'] as List<dynamic>? ?? []).map((e) => e.toString()).toList();
    final arrow = widget.isLeft ? '←' : '→';

    return LiftIn(
      offsetY: 10,
      duration: const Duration(milliseconds: 280),
      child: RepaintBoundary(
        child: ValueListenableBuilder<Color>(
          valueListenable: app.lampColorNotifier,
          builder: (_, appLamp, _) {
            final lamp = _cardLamp ?? appLamp;
            return ValueListenableBuilder<ArtworkModel?>(
              valueListenable: app.artworkNotifier(m.key),
              builder: (_, art, _) {
                final bgSources = <String>[
                  if (art?.backdrop != null && art!.backdrop!.isNotEmpty) art.backdrop!,
                  if (m.backdrop.isNotEmpty) m.backdrop,
                  if (m.poster.isNotEmpty) m.poster,
                ];
                final logo = (art?.logo?.isNotEmpty == true) ? art!.logo! : (m.logo.isNotEmpty ? m.logo : null);

                final loseDx = widget.isLeft ? -36.0 : 36.0;
                final loseRot = widget.isLeft ? -0.028 : 0.028;

                return MouseRegion(
                  cursor: SystemMouseCursors.click,
                  onEnter: (_) => setState(() => _hovered = true),
                  onHover: (e) => _updateTilt(context, e.localPosition),
                  onExit: (_) {
                    _tilt.value = Offset.zero;
                    setState(() => _hovered = false);
                  },
                  child: GestureDetector(
                    onHorizontalDragStart: (_) => setState(() => _dragging = true),
                    onHorizontalDragUpdate: (d) => _dragX.value = (_dragX.value + d.delta.dx).clamp(-140.0, 140.0),
                    onHorizontalDragEnd: (_) {
                      final dx = _dragX.value;
                      setState(() => _dragging = false);
                      _dragX.value = 0.0;
                      if (dx.abs() >= 72.0) {
                        widget.onKeep();
                      }
                    },
                    onHorizontalDragCancel: () {
                      setState(() => _dragging = false);
                      _dragX.value = 0.0;
                    },
                    child: ValueListenableBuilder<Offset>(
                      valueListenable: _tilt,
                      builder: (_, tilt, child) => ValueListenableBuilder<double>(
                        valueListenable: _dragX,
                        builder: (_, dragX, inner) {
                          final m4 = Matrix4.identity()
                            ..setEntry(3, 2, 0.0007)
                            ..rotateX(-tilt.dy * 0.024)
                            ..rotateY(tilt.dx * 0.024)
                            ..translateByDouble(dragX * 0.35, 0.0, 0.0, 1.0)
                            ..rotateZ(dragX * 0.00045);
                          return AnimatedContainer(
                            duration: Duration(milliseconds: _dragging ? 0 : 220),
                            curve: _dragging ? Curves.linear : VeyloCurves.spring,
                            transform: m4,
                            transformAlignment: Alignment.center,
                            child: inner,
                          );
                        },
                        child: child,
                      ),
                      child: AnimatedOpacity(
                        duration: const Duration(milliseconds: 200),
                        curve: Curves.easeInCubic,
                        opacity: widget.state == -1 ? 0.0 : 1.0,
                        child: AnimatedContainer(
                          duration: Duration(milliseconds: widget.state == 1 ? 240 : 200),
                          curve: widget.state == 1 ? VeyloCurves.spring : VeyloCurves.out,
                          transform: Matrix4.identity()
                            ..translateByDouble(
                              widget.state == -1 ? loseDx : 0.0,
                              _hovered ? -4.0 : 0.0,
                              0.0,
                              1.0,
                            )
                            ..rotateZ(widget.state == -1 ? loseRot : 0.0)
                            ..scaleByDouble(
                              widget.state == 1 ? 1.024 : 1.0,
                              widget.state == 1 ? 1.024 : 1.0,
                              1.0,
                              1.0,
                            ),
                          transformAlignment: Alignment.center,
                          decoration: BoxDecoration(
                            borderRadius: BorderRadius.circular(28),
                            boxShadow: const [
                              BoxShadow(
                                color: Color.fromRGBO(0, 0, 0, 0.56),
                                blurRadius: 48,
                                offset: Offset(0, 22),
                              ),
                            ],
                          ),
                          child: ClipRRect(
                            borderRadius: BorderRadius.circular(28),
                            child: Stack(
                              fit: StackFit.expand,
                              children: [
                                GestureDetector(
                                  onTap: widget.onKeep,
                                  child: Stack(
                                    fit: StackFit.expand,
                                    children: [
                                      AnimatedScale(
                                        scale: _hovered ? 1.035 : 1.0,
                                        duration: const Duration(milliseconds: 450),
                                        curve: VeyloCurves.out,
                                        child: FallbackImage(sources: bgSources, cacheWidth: 900),
                                      ),
                                      const DecoratedBox(
                                        decoration: BoxDecoration(
                                          gradient: LinearGradient(
                                            begin: Alignment.topCenter,
                                            end: Alignment.bottomCenter,
                                            stops: [0.0, 0.24, 0.52, 1.0],
                                            colors: [
                                              Color.fromRGBO(0, 0, 0, 0.38),
                                              Color.fromRGBO(0, 0, 0, 0.04),
                                              Color.fromRGBO(0, 0, 0, 0.42),
                                              Color.fromRGBO(0, 0, 0, 0.92),
                                            ],
                                      ),
                                    ),
                                  ),
                                ],
                              ),
                            ),

                            // Top bar: rolling match badge + streak on left, Save / Like / Dislike orbs on right
                            Positioned(
                              top: 16,
                              left: 16,
                              right: 16,
                              child: Row(
                                mainAxisAlignment: MainAxisAlignment.spaceBetween,
                                children: [
                                  Wrap(
                                    spacing: 8,
                                    children: [
                                      if (matching != null)
                                        TweenAnimationBuilder<double>(
                                          tween: Tween(begin: 0, end: matching.toDouble()),
                                          duration: const Duration(milliseconds: 600),
                                          curve: VeyloCurves.out,
                                          builder: (_, val, _) => ClipRRect(
                                            borderRadius: BorderRadius.circular(18),
                                            child: GlassBox(
                                              borderRadius: 18,
                                              lamp: lamp,
                                              child: Stack(
                                                children: [
                                                  Padding(
                                                    padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 7),
                                                    child: Row(
                                                      mainAxisSize: MainAxisSize.min,
                                                      children: [
                                                        Text(
                                                          '${val.round()}%',
                                                          style: VeyloText.ui(17, weight: 800, tabular: true, height: 1.0),
                                                        ),
                                                        const SizedBox(width: 5),
                                                        Text('match', style: VeyloText.ui(13, weight: 600, color: VeyloColors.dust)),
                                                      ],
                                                    ),
                                                  ),
                                                  Positioned(
                                                    left: 0,
                                                    right: 0,
                                                    bottom: 0,
                                                    child: SizedBox(
                                                      height: 3,
                                                      child: Align(
                                                        alignment: Alignment.centerLeft,
                                                        child: FractionallySizedBox(
                                                          widthFactor: (val / 100.0).clamp(0.0, 1.0),
                                                          child: ColoredBox(color: lamp),
                                                        ),
                                                      ),
                                                    ),
                                                  ),
                                                ],
                                              ),
                                            ),
                                          ),
                                        ),
                                      if (wins > 0)
                                        Container(
                                          height: 36,
                                          padding: const EdgeInsets.symmetric(horizontal: 13),
                                          decoration: BoxDecoration(
                                            color: VeyloColors.amber,
                                            borderRadius: BorderRadius.circular(18),
                                          ),
                                          child: Row(
                                            mainAxisSize: MainAxisSize.min,
                                            children: [
                                              const VeyloIcon('Trophy', size: 17, color: Color(0xFF000000)),
                                              const SizedBox(width: 6),
                                              Text(
                                                '$wins',
                                                style: VeyloText.ui(15, weight: 800, color: const Color(0xFF000000), tabular: true),
                                              ),
                                            ],
                                          ),
                                        ),
                                    ],
                                  ),
                                  Row(
                                    children: [
                                      VeyloOrb(
                                        icon: 'Bookmark',
                                        size: 44,
                                        active: _saved,
                                        tooltip: _saved ? 'Saved to Library' : 'Save to Library',
                                        onTap: () async {
                                          setState(() => _saved = !_saved);
                                          await app.bridge.setSaved(m, _saved);
                                          app.bumpPicksDirty();
                                        },
                                      ),
                                      const SizedBox(width: 8),
                                      VeyloOrb(
                                        icon: 'ThumbUp',
                                        size: 44,
                                        active: _liked,
                                        tooltip: _liked ? 'Liked' : 'Like',
                                        onTap: () async {
                                          setState(() => _liked = !_liked);
                                          await app.bridge.feedback(m, _liked ? 1 : 0);
                                          app.bumpPicksDirty();
                                        },
                                      ),
                                      const SizedBox(width: 8),
                                      VeyloOrb(
                                        icon: 'ThumbDown',
                                        size: 44,
                                        tooltip: 'Not for me',
                                        onTap: () async {
                                          await app.bridge.feedback(m, -1);
                                          app.bumpPicksDirty();
                                          widget.onRefresh();
                                        },
                                      ),
                                    ],
                                  ),
                                ],
                              ),
                            ),

                            // Bottom copy & actions (.duel-copy)
                            Positioned(
                              left: 26,
                              right: 26,
                              bottom: 24,
                              child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  if (why != null && why.isNotEmpty) ...[
                                    Row(
                                      children: [
                                        const VeyloIcon('Sparkles', size: 16, color: VeyloColors.bone, filled: true),
                                        const SizedBox(width: 7),
                                        Expanded(
                                          child: Text(
                                            why,
                                            maxLines: 1,
                                            overflow: TextOverflow.ellipsis,
                                            style: VeyloText.ui(14.5, weight: 600),
                                          ),
                                        ),
                                      ],
                                    ),
                                    const SizedBox(height: 10),
                                  ],
                                  if (logo != null)
                                    ConstrainedBox(
                                      constraints: const BoxConstraints(maxHeight: 68, maxWidth: 320),
                                      child: CachedNetworkImage(
                                        imageUrl: logo,
                                        fit: BoxFit.contain,
                                        alignment: Alignment.bottomLeft,
                                        memCacheWidth: 640,
                                        errorWidget: (_, _, _) => _titleText(m.name),
                                      ),
                                    )
                                  else
                                    _titleText(m.name),
                                  const SizedBox(height: 12),
                                  // Single-row facts matching CSS `.facts`
                                  SingleChildScrollView(
                                    scrollDirection: Axis.horizontal,
                                    child: Row(
                                      children: [
                                        if (m.rating.isNotEmpty) ...[
                                          ScoresPill(imdb: m.rating),
                                          const SizedBox(width: 8),
                                        ],
                                        if (m.runtime.isNotEmpty) ...[
                                          FactPill(
                                            child: Row(
                                              mainAxisSize: MainAxisSize.min,
                                              children: [
                                                const VeyloIcon('Clock', size: 16, color: VeyloColors.dust),
                                                const SizedBox(width: 6),
                                                Text(m.runtime),
                                              ],
                                            ),
                                          ),
                                          const SizedBox(width: 8),
                                        ],
                                        if (m.year.isNotEmpty) ...[
                                          FactPill(
                                            child: Row(
                                              mainAxisSize: MainAxisSize.min,
                                              children: [
                                                const VeyloIcon('Calendar', size: 16, color: VeyloColors.dust),
                                                const SizedBox(width: 6),
                                                Text(m.year, style: const TextStyle(fontFeatures: VeyloText.tnum)),
                                              ],
                                            ),
                                          ),
                                          const SizedBox(width: 8),
                                        ],
                                        if (m.genres.isNotEmpty)
                                          FactPill(
                                            child: Row(
                                              mainAxisSize: MainAxisSize.min,
                                              children: [
                                                const VeyloIcon('Tag', size: 16, color: VeyloColors.dust),
                                                const SizedBox(width: 6),
                                                Text(m.genres.take(2).join(', ')),
                                              ],
                                            ),
                                          ),
                                      ],
                                    ),
                                  ),
                                  if (feels.isNotEmpty) ...[
                                    const SizedBox(height: 10),
                                    Wrap(
                                      spacing: 6,
                                      crossAxisAlignment: WrapCrossAlignment.center,
                                      children: [
                                        Text('Feels', style: VeyloText.ui(13, weight: 500, color: VeyloColors.dust)),
                                        for (final f in feels.take(3))
                                          Container(
                                            height: 26,
                                            padding: const EdgeInsets.symmetric(horizontal: 10),
                                            alignment: Alignment.center,
                                            decoration: BoxDecoration(
                                              color: const Color.fromRGBO(255, 255, 255, 0.12),
                                              borderRadius: BorderRadius.circular(13),
                                            ),
                                            child: Text(f, style: VeyloText.ui(13, weight: 600, color: VeyloColors.bone)),
                                          ),
                                      ],
                                    ),
                                  ],
                                  const SizedBox(height: 16),
                                  Wrap(
                                    spacing: 10,
                                    runSpacing: 8,
                                    children: [
                                      VeyloPill(
                                        label: 'Keep',
                                        icon: 'Check',
                                        accent: true,
                                        lamp: lamp,
                                        trailing: Keycap(arrow, dark: true, small: true),
                                        onTap: widget.onKeep,
                                      ),
                                      VeyloPill(
                                        label: m.inProgress ? 'Resume' : 'Play',
                                        icon: 'Play',
                                        onTap: () => app.autoplayMedia(m),
                                      ),
                                      VeyloOrb(
                                        icon: 'Info',
                                        tooltip: 'Details',
                                        onTap: () => app.openTitle(m),
                                      ),
                                    ],
                                  ),
                                ],
                              ),
                            ),

                                // Specular glass rim
                                IgnorePointer(
                                  child: AnimatedContainer(
                                    duration: const Duration(milliseconds: 200),
                                    curve: VeyloCurves.out,
                                    decoration: BoxDecoration(
                                      borderRadius: BorderRadius.circular(28),
                                      border: Border.all(
                                        color: widget.state == 1
                                            ? VeyloColors.mixLamp(lamp, VeyloColors.bone, 0.7)
                                            : (_hovered
                                                ? VeyloColors.mixLamp(lamp, VeyloColors.bone, 0.55)
                                                : const Color.fromRGBO(255, 255, 255, 0.12)),
                                        width: (widget.state == 1 || _hovered) ? 1.5 : 1,
                                      ),
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
                );
              },
            );
          },
        ),
      ),
    );
  }

  Widget _titleText(String name) => Text(
        name,
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
        style: VeyloText.display(36, weight: 800, height: 0.98),
      );
}

class _ShortItem extends StatefulWidget {
  const _ShortItem({required this.entry, required this.onRemove});
  final Map<String, dynamic> entry;
  final VoidCallback onRemove;

  @override
  State<_ShortItem> createState() => _ShortItemState();
}

class _ShortItemState extends State<_ShortItem> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final m = MediaItem.fromJson(widget.entry['media'] as Map<String, dynamic>);
    final star = widget.entry['star'] as bool? ?? false;
    final pct = (widget.entry['pct'] as num?)?.toInt() ?? 50;

    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: SurfaceBox(
        borderRadius: 18,
        hover: _hovered,
        padding: const EdgeInsets.fromLTRB(6, 6, 8, 6),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            GestureDetector(
              onTap: () => app.openTitle(m),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  SizedBox(
                    width: 92,
                    height: 52,
                    child: Stack(
                      fit: StackFit.expand,
                      children: [
                        ClipRRect(
                          borderRadius: BorderRadius.circular(12),
                          child: FallbackImage(sources: [m.backdrop, ...m.posters], cacheWidth: 220),
                        ),
                        Positioned(
                          left: 6,
                          right: 6,
                          bottom: 3,
                          child: Container(
                            height: 4,
                            decoration: BoxDecoration(
                              color: const Color.fromRGBO(0, 0, 0, 0.55),
                              borderRadius: BorderRadius.circular(2),
                            ),
                            alignment: Alignment.centerLeft,
                            child: ValueListenableBuilder<Color>(
                              valueListenable: app.lampColorNotifier,
                              builder: (_, lamp, _) => TweenAnimationBuilder<double>(
                                tween: Tween(begin: 0, end: (pct / 100.0).clamp(0.0, 1.0)),
                                duration: const Duration(milliseconds: 450),
                                curve: VeyloCurves.out,
                                builder: (_, v, _) => FractionallySizedBox(
                                  widthFactor: v,
                                  child: Container(
                                    decoration: BoxDecoration(
                                      color: lamp,
                                      borderRadius: BorderRadius.circular(2),
                                    ),
                                  ),
                                ),
                              ),
                            ),
                          ),
                        ),
                      ],
                    ),
                  ),
                  const SizedBox(width: 10),
                  ConstrainedBox(
                    constraints: const BoxConstraints(maxWidth: 130),
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        if (star) ...[
                          const VeyloIcon('Star', size: 14, color: VeyloColors.amber, filled: true),
                          const SizedBox(width: 5),
                        ],
                        Flexible(
                          child: Text(
                            m.name,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: VeyloText.ui(14, weight: 600),
                          ),
                        ),
                      ],
                    ),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 8),
            VeyloOrb(
              icon: 'Play',
              size: 36,
              iconSize: 15,
              backgroundOverride: const Color.fromRGBO(255, 255, 255, 0.09),
              onTap: () => app.autoplayMedia(m),
            ),
            const SizedBox(width: 4),
            VeyloOrb(
              icon: 'Close',
              size: 36,
              iconSize: 15,
              backgroundOverride: const Color.fromRGBO(255, 255, 255, 0.09),
              onTap: widget.onRemove,
            ),
          ],
        ),
      ),
    );
  }
}
