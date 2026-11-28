import 'dart:async';
import 'dart:io' show Platform;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:material_ui/material_ui.dart' show InputDecoration, TextField;
import '../bridge/veylo_bridge.dart';
import '../state/app_state.dart';
import '../theme/veylo_theme.dart';
import '../widgets/glass.dart';
import '../widgets/veylo_icons.dart';

enum _SettingsCat { all, account, playback, addons, display, privacy }
enum _AddonSetupTab { link, bundle, stremio, directory }
enum _AccountPanel { choices, email, qr, linkSender }

class SettingsScreen extends StatefulWidget {
  const SettingsScreen({super.key});

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

class _SettingsScreenState extends State<SettingsScreen> {
  final ScrollController _scroll = ScrollController();
  final TextEditingController _addonUrlCtrl = TextEditingController();
  final FocusNode _addonUrlFocus = FocusNode();
  final TextEditingController _stremioEmailCtrl = TextEditingController();
  final TextEditingController _stremioPassCtrl = TextEditingController();
  final TextEditingController _bundleCtrl = TextEditingController();
  final TextEditingController _dirUrlCtrl = TextEditingController();
  final TextEditingController _histImportCtrl = TextEditingController();
  final FocusNode _stremioEmailFocus = FocusNode();
  final FocusNode _stremioPassFocus = FocusNode();
  final FocusNode _bundleFocus = FocusNode();
  final FocusNode _dirUrlFocus = FocusNode();
  final FocusNode _histImportFocus = FocusNode();

  _SettingsCat _cat = _SettingsCat.all;
  _AddonSetupTab _addonTab = _AddonSetupTab.link;
  Map<String, dynamic> _settings = {};
  Map<String, dynamic> _accountState = {};
  List<Map<String, dynamic>> _languages = [];
  List<Map<String, dynamic>> _addons = [];
  List<Map<String, dynamic>> _dirEntries = [];
  bool _dirLoaded = false;
  bool _histImportOpen = false;
  String? _diagReport;
  bool _crashReportsAvailable = false;
  bool _loading = true;
  bool _busy = false;
  bool _showUrl = false;
  bool _armResetTaste = false;
  int? _armRemoveAddon;
  String? _note;
  String? _problem;

  @override
  void initState() {
    super.initState();
    _addonUrlFocus.addListener(_onFocus);
    _stremioEmailFocus.addListener(_onFocus);
    _stremioPassFocus.addListener(_onFocus);
    _bundleFocus.addListener(_onFocus);
    _dirUrlFocus.addListener(_onFocus);
    _histImportFocus.addListener(_onFocus);
    _scroll.addListener(_onScroll);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      unawaited(_load());
    });
  }

  void _onFocus() {
    if (mounted) setState(() {});
  }

  void _onScroll() {
    if (mounted) AppStateScope.read(context).setScrolled(_scroll.offset > 4);
  }

  @override
  void dispose() {
    _addonUrlFocus.removeListener(_onFocus);
    _stremioEmailFocus.removeListener(_onFocus);
    _stremioPassFocus.removeListener(_onFocus);
    _bundleFocus.removeListener(_onFocus);
    _dirUrlFocus.removeListener(_onFocus);
    _histImportFocus.removeListener(_onFocus);
    _scroll.removeListener(_onScroll);
    _scroll.dispose();
    _addonUrlCtrl.dispose();
    _addonUrlFocus.dispose();
    _stremioEmailCtrl.dispose();
    _stremioPassCtrl.dispose();
    _bundleCtrl.dispose();
    _dirUrlCtrl.dispose();
    _histImportCtrl.dispose();
    _stremioEmailFocus.dispose();
    _stremioPassFocus.dispose();
    _bundleFocus.dispose();
    _dirUrlFocus.dispose();
    _histImportFocus.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    final app = AppStateScope.read(context);
    try {
      final results = await Future.wait([
        app.bridge.settingsGet(),
        app.bridge.addonsOverview(),
        app.bridge.accountState(),
        app.bridge.crashReporting().catchError((_) => <String, dynamic>{}),
      ]);
      if (!mounted) return;
      final sRes = results[0];
      final aRes = results[1];
      final accRes = results[2];
      app.setAddonUpdates((aRes['updates'] as num?)?.toInt() ?? 0);
      app.accountsAvailable = accRes['accounts_available'] as bool? ?? app.accountsAvailable;
      app.googleAvailable = accRes['google_available'] as bool? ?? app.googleAvailable;
      app.syncKeyNeededNotifier.value = accRes['sync_key_needed'] as bool? ?? false;
      setState(() {
        _loading = false;
        _settings = (sRes['settings'] as Map<String, dynamic>?) ?? {};
        _languages = (sRes['languages'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
        _addons = (aRes['addons'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
        _accountState = accRes;
        _crashReportsAvailable = results[3]['available'] as bool? ?? false;
      });
    } catch (e) {
      if (mounted) {
        setState(() {
          _loading = false;
          _problem = e.toString();
        });
      }
    }
  }

  Future<void> _patch(Map<String, dynamic> patch) async {
    final app = AppStateScope.read(context);
    setState(() => _settings = {..._settings, ...patch});
    try {
      final res = await app.bridge.settingsUpdate(patch);
      if (mounted) {
        setState(() => _settings = (res['settings'] as Map<String, dynamic>?) ?? _settings);
      }
      if (patch.containsKey('better_posters')) {
        unawaited(app.loadHome(force: true));
      }
    } catch (_) {}
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final gutter = VeyloLayout.gutter(MediaQuery.sizeOf(context).width);
    final insets = MediaQuery.viewPaddingOf(context);
    bool shows(_SettingsCat c) => _cat == _SettingsCat.all || _cat == c;

    return SingleChildScrollView(
      controller: _scroll,
      padding: EdgeInsets.fromLTRB(gutter, 92, gutter, 72) + insets,
      child: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 860),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                mainAxisAlignment: MainAxisAlignment.spaceBetween,
                crossAxisAlignment: CrossAxisAlignment.baseline,
                textBaseline: TextBaseline.alphabetic,
                children: [
                  const Text('Settings', style: VeyloText.title),
                  Container(
                    padding: const EdgeInsets.symmetric(horizontal: 11, vertical: 4),
                    decoration: BoxDecoration(
                      color: const Color.fromRGBO(255, 255, 255, 0.06),
                      borderRadius: BorderRadius.circular(12),
                    ),
                    child: Text('v${app.version}', style: VeyloText.ui(13, weight: 600, color: VeyloColors.dust, tabular: true)),
                  ),
                ],
              ),
              const SizedBox(height: 18),

              // Category nav pills
              ValueListenableBuilder<int>(
                valueListenable: app.addonUpdatesNotifier,
                builder: (_, updates, _) => ValueListenableBuilder<bool>(
                  valueListenable: app.syncKeyNeededNotifier,
                  builder: (_, keyNeeded, _) {
                    return Wrap(
                      spacing: 8,
                      runSpacing: 8,
                      children: [
                        for (final (cat, icon, label) in [
                          (_SettingsCat.all, 'Sliders', 'All'),
                          (_SettingsCat.account, 'Users', 'Account'),
                          (_SettingsCat.playback, 'Play', 'Playback'),
                          (_SettingsCat.addons, 'Stack', 'Add-ons'),
                          (_SettingsCat.display, 'Tv', 'Display'),
                          (_SettingsCat.privacy, 'Lock', 'Privacy & About'),
                        ])
                          _NavPill(
                            icon: icon,
                            label: label,
                            active: _cat == cat,
                            hasDot: (cat == _SettingsCat.addons && updates > 0) ||
                                (cat == _SettingsCat.account && keyNeeded),
                            dotColor: cat == _SettingsCat.account && keyNeeded ? VeyloColors.amber : null,
                            onTap: () => setState(() => _cat = cat),
                          ),
                      ],
                    );
                  },
                ),
              ),

              if (_loading) ...[
                const SizedBox(height: 28),
                const SkeletonBox(height: 180, borderRadius: 22),
              ] else ...[
                // Account & Sync
                if (shows(_SettingsCat.account)) ...[
                  const SizedBox(height: 26),
                  _AccountBlock(
                    initialState: _accountState,
                    onStateChanged: (nextState, {bool reloadData = false}) {
                      if (!mounted) return;
                      app.syncKeyNeededNotifier.value = nextState['sync_key_needed'] as bool? ?? false;
                      setState(() => _accountState = nextState);
                      if (reloadData) {
                        unawaited(_load());
                        app.bumpPicksDirty();
                        unawaited(app.loadHome(force: true));
                      }
                    },
                  ),
                ],

                // Playback
                if (shows(_SettingsCat.playback)) ...[
                  const SizedBox(height: 26),
                  const GroupHead(icon: 'Play', title: 'Playback'),
                  const SizedBox(height: 10),
                  _SettingsGroup(
                    children: [
                      _SettingRow(
                        name: 'Audio language',
                        about: 'Smart Play prefers streams and embedded audio tracks in this language.',
                        control: _GlassDropdown(
                          value: _settings['stream_language']?.toString() ?? 'english',
                          options: [for (final l in _languages) (l['key'].toString(), l['label'].toString())],
                          onChanged: (v) => _patch({'stream_language': v}),
                        ),
                      ),
                      _SettingRow(
                        name: 'Max streaming bitrate',
                        about: _bitrateDesc(),
                        control: _GlassDropdown(
                          value: _settings['max_bitrate_mbps'] == null ? 'auto' : _settings['max_bitrate_mbps'].toString(),
                          options: const [
                            ('auto', 'Auto (match speed)'),
                            ('0', 'Unlimited'),
                            ('60', '60 Mbps (4K high)'),
                            ('30', '30 Mbps (4K)'),
                            ('15', '15 Mbps (1080p high)'),
                            ('8', '8 Mbps (data saver)'),
                          ],
                          onChanged: (v) => _patch({'max_bitrate_mbps': v == 'auto' ? null : int.tryParse(v)}),
                        ),
                      ),
                      _SettingRow(
                        name: 'Up Next countdown',
                        about: 'Show a short countdown to the next episode at credits or episode end.',
                        control: VeyloSegmented<bool>(
                          compact: true,
                          value: _settings['auto_next'] as bool? ?? true,
                          options: const [(true, 'On', null), (false, 'Off', null)],
                          onChanged: (v) => _patch({'auto_next': v}),
                        ),
                      ),
                      _SettingRow(
                        name: 'Skip intros automatically',
                        about: 'Skips intros and recaps the first time they play; a button lets you undo.',
                        control: VeyloSegmented<bool>(
                          compact: true,
                          value: _settings['auto_skip_intro'] as bool? ?? true,
                          options: const [(true, 'On', null), (false, 'Off', null)],
                          onChanged: (v) => _patch({'auto_skip_intro': v}),
                        ),
                      ),
                      _SettingRow(
                        name: 'HDMI audio passthrough',
                        about: 'Send Dolby Digital, DTS, and TrueHD bitstreams directly to an AVR or soundbar.',
                        control: VeyloSegmented<bool>(
                          compact: true,
                          value: _settings['audio_passthrough'] as bool? ?? false,
                          options: const [(true, 'On', null), (false, 'Off', null)],
                          onChanged: (v) => _patch({'audio_passthrough': v}),
                        ),
                      ),
                      _SettingRow(
                        name: 'Night mode (volume leveling)',
                        about: 'Soften loud explosions and boost quiet dialogue using dynamic range normalization.',
                        control: VeyloSegmented<bool>(
                          compact: true,
                          value: _settings['audio_night_mode'] as bool? ?? false,
                          options: const [(true, 'On', null), (false, 'Off', null)],
                          onChanged: (v) => _patch({'audio_night_mode': v}),
                        ),
                      ),
                      _SettingRow(
                        name: 'Prefetch sources while browsing',
                        about: 'When off, stream add-ons are only queried after you press Play.',
                        badge: (_settings['prefetch_sources'] as bool? ?? false) ? null : 'Private default',
                        control: VeyloSegmented<bool>(
                          compact: true,
                          value: _settings['prefetch_sources'] as bool? ?? false,
                          options: const [(true, 'On', null), (false, 'Off', null)],
                          onChanged: (v) => _patch({'prefetch_sources': v}),
                        ),
                      ),
                      if (app.demoStream.isNotEmpty)
                        _SettingRow(
                          name: 'Test player',
                          about: 'Open the native libmpv player right away with a sample stream to test controls, audio, and subtitles.',
                          control: VeyloPill(
                            label: 'Open player',
                            icon: 'Play',
                            accent: true,
                            small: true,
                            onTap: () => unawaited(app.playDemo()),
                          ),
                        ),
                    ],
                  ),
                ],

                // Add-ons
                if (shows(_SettingsCat.addons)) ...[
                  const SizedBox(height: 26),
                  GroupHead(
                    icon: 'Stack',
                    title: 'Add-ons',
                    trailing: VeyloPill(
                      label: _busy ? 'Checking…' : 'Check updates',
                      icon: 'Refresh',
                      small: true,
                      disabled: _busy,
                      onTap: () async {
                        setState(() {
                          _busy = true;
                          _problem = null;
                          _note = null;
                        });
                        try {
                          final res = await app.bridge.addonsCheck();
                          if (mounted) setState(() => _note = res['note']?.toString());
                          await _load();
                        } catch (e) {
                          if (mounted) setState(() => _problem = e.toString());
                        } finally {
                          if (mounted) setState(() => _busy = false);
                        }
                      },
                    ),
                  ),
                  const SizedBox(height: 10),
                  _SettingsGroup(
                    children: [
                      for (final a in _addons)
                        _AddonRow(
                          addon: a,
                          busy: _busy,
                          armed: _armRemoveAddon == a['idx'],
                          onArm: () => setState(() => _armRemoveAddon = a['idx'] as int?),
                          onCancelArm: () => setState(() => _armRemoveAddon = null),
                          onConfigure: () => app.bridge.addonConfigure(a['idx'] as int),
                          onUpdate: () async {
                            setState(() {
                              _busy = true;
                              _problem = null;
                            });
                            try {
                              await app.bridge.addonUpdate(a['idx'] as int);
                              await _load();
                            } catch (e) {
                              if (mounted) setState(() => _problem = e.toString());
                            } finally {
                              if (mounted) setState(() => _busy = false);
                            }
                          },
                          onRemove: () async {
                            setState(() {
                              _armRemoveAddon = null;
                              _busy = true;
                              _problem = null;
                            });
                            try {
                              await app.bridge.addonRemove(a['idx'] as int);
                              await _load();
                              unawaited(app.loadHome(force: true));
                            } catch (e) {
                              if (mounted) setState(() => _problem = e.toString());
                            } finally {
                              if (mounted) setState(() => _busy = false);
                            }
                          },
                        ),
                      // Install / import add-on drawer
                      Container(
                        width: double.infinity,
                        padding: const EdgeInsets.all(20),
                        color: const Color.fromRGBO(0, 0, 0, 0.22),
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Wrap(
                              spacing: 12,
                              runSpacing: 10,
                              alignment: WrapAlignment.spaceBetween,
                              crossAxisAlignment: WrapCrossAlignment.center,
                              children: [
                                Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    Text(
                                      switch (_addonTab) {
                                        _AddonSetupTab.link => 'Add an add-on link',
                                        _AddonSetupTab.bundle => 'Share or import a bundle',
                                        _AddonSetupTab.stremio => 'Import from Stremio',
                                        _AddonSetupTab.directory => 'Browse a community directory',
                                      },
                                      style: VeyloText.ui(15, weight: 600),
                                    ),
                                    const SizedBox(height: 3),
                                    Text(
                                      switch (_addonTab) {
                                        _AddonSetupTab.link => 'Links stay masked and are never logged.',
                                        _AddonSetupTab.bundle => 'Copy your add-on list as a portable bundle, or paste one from another device.',
                                        _AddonSetupTab.stremio => 'One-time sign-in to copy your Stremio add-ons. Credentials are never saved.',
                                        _AddonSetupTab.directory => 'Load any user-supplied https:// Stremio directory JSON URL to browse and install.',
                                      },
                                      style: VeyloText.ui(13.5, weight: 500, color: VeyloColors.dust),
                                    ),
                                  ],
                                ),
                                VeyloSegmented<_AddonSetupTab>(
                                  compact: true,
                                  value: _addonTab,
                                  options: const [
                                    (_AddonSetupTab.link, 'Link', null),
                                    (_AddonSetupTab.bundle, 'Bundle', null),
                                    (_AddonSetupTab.stremio, 'Stremio', null),
                                    (_AddonSetupTab.directory, 'Directory', null),
                                  ],
                                  onChanged: (v) => setState(() {
                                    _addonTab = v;
                                    _problem = null;
                                    _note = null;
                                  }),
                                ),
                              ],
                            ),
                            const SizedBox(height: 12),
                            if (_addonTab == _AddonSetupTab.link)
                              Wrap(
                                spacing: 8,
                                runSpacing: 8,
                                crossAxisAlignment: WrapCrossAlignment.center,
                                children: [
                                  ValueListenableBuilder<Color>(
                                    valueListenable: app.lampColorNotifier,
                                    builder: (_, lamp, _) => _SettingsField(
                                      controller: _addonUrlCtrl,
                                      focusNode: _addonUrlFocus,
                                      lamp: lamp,
                                      width: 340,
                                      obscureText: !_showUrl,
                                      hintText: 'stremio://… or https://…/manifest.json',
                                      onChanged: (_) => setState(() {}),
                                    ),
                                  ),
                                  VeyloPill(
                                    label: _showUrl ? 'Hide' : 'Show',
                                    small: true,
                                    onTap: () => setState(() => _showUrl = !_showUrl),
                                  ),
                                  VeyloPill(
                                    label: 'Install',
                                    icon: 'Plus',
                                    accent: true,
                                    small: true,
                                    disabled: _busy || _addonUrlCtrl.text.trim().isEmpty,
                                    onTap: () async {
                                      final u = _addonUrlCtrl.text.trim();
                                      setState(() {
                                        _busy = true;
                                        _problem = null;
                                        _note = null;
                                      });
                                      try {
                                        final res = await app.bridge.addonInstall(u);
                                        unawaited(app.loadHome(force: true));
                                        if (!mounted) return;
                                        _addonUrlCtrl.clear();
                                        setState(() => _note = 'Installed ${res['name']}.');
                                        await _load();
                                      } catch (e) {
                                        if (mounted) setState(() => _problem = e.toString());
                                      } finally {
                                        if (mounted) setState(() => _busy = false);
                                      }
                                    },
                                  ),
                                ],
                              )
                            else if (_addonTab == _AddonSetupTab.bundle)
                              Wrap(
                                spacing: 8,
                                runSpacing: 8,
                                crossAxisAlignment: WrapCrossAlignment.center,
                                children: [
                                  ValueListenableBuilder<Color>(
                                    valueListenable: app.lampColorNotifier,
                                    builder: (_, lamp, _) => _SettingsField(
                                      controller: _bundleCtrl,
                                      focusNode: _bundleFocus,
                                      lamp: lamp,
                                      width: 340,
                                      obscureText: true,
                                      hintText: 'Paste bundle JSON or manifest links…',
                                      onChanged: (_) => setState(() {}),
                                    ),
                                  ),
                                  VeyloPill(
                                    label: 'Import bundle',
                                    icon: 'Download',
                                    accent: true,
                                    small: true,
                                    disabled: _busy || _bundleCtrl.text.trim().isEmpty,
                                    onTap: () async {
                                      final raw = _bundleCtrl.text.trim();
                                      setState(() {
                                        _busy = true;
                                        _problem = null;
                                        _note = null;
                                      });
                                      try {
                                        final res = await app.bridge.addonImportBundle(raw);
                                        unawaited(app.loadHome(force: true));
                                        if (!mounted) return;
                                        _bundleCtrl.clear();
                                        final ok = (res['ok'] as num?)?.toInt() ?? 0;
                                        final failed = (res['failed'] as num?)?.toInt() ?? 0;
                                        setState(() => _note = failed > 0
                                            ? 'Imported $ok add-on(s); $failed failed.'
                                            : 'Imported $ok add-on(s).');
                                        await _load();
                                      } catch (e) {
                                        if (mounted) setState(() => _problem = e.toString());
                                      } finally {
                                        if (mounted) setState(() => _busy = false);
                                      }
                                    },
                                  ),
                                  VeyloPill(
                                    label: 'Copy my bundle',
                                    icon: 'Copy',
                                    small: true,
                                    disabled: _busy,
                                    onTap: () async {
                                      setState(() {
                                        _busy = true;
                                        _problem = null;
                                        _note = null;
                                      });
                                      try {
                                        final res = await app.bridge.addonExportBundle();
                                        final count = (res['count'] as num?)?.toInt() ?? 0;
                                        final bundle = res['bundle']?.toString() ?? '';
                                        await Clipboard.setData(ClipboardData(text: bundle));
                                        if (mounted) setState(() => _note = 'Copied $count add-on(s) to clipboard.');
                                      } catch (e) {
                                        if (mounted) setState(() => _problem = e.toString());
                                      } finally {
                                        if (mounted) setState(() => _busy = false);
                                      }
                                    },
                                  ),
                                ],
                              )
                            else if (_addonTab == _AddonSetupTab.stremio)
                              Wrap(
                                spacing: 8,
                                runSpacing: 8,
                                crossAxisAlignment: WrapCrossAlignment.center,
                                children: [
                                  ValueListenableBuilder<Color>(
                                    valueListenable: app.lampColorNotifier,
                                    builder: (_, lamp, _) => _SettingsField(
                                      controller: _stremioEmailCtrl,
                                      focusNode: _stremioEmailFocus,
                                      lamp: lamp,
                                      width: 240,
                                      hintText: 'Stremio email',
                                      onChanged: (_) => setState(() {}),
                                    ),
                                  ),
                                  ValueListenableBuilder<Color>(
                                    valueListenable: app.lampColorNotifier,
                                    builder: (_, lamp, _) => _SettingsField(
                                      controller: _stremioPassCtrl,
                                      focusNode: _stremioPassFocus,
                                      lamp: lamp,
                                      width: 210,
                                      obscureText: true,
                                      hintText: 'Stremio password',
                                      onChanged: (_) => setState(() {}),
                                    ),
                                  ),
                                  VeyloPill(
                                    label: _busy ? 'Importing…' : 'Import add-ons',
                                    icon: 'Download',
                                    accent: true,
                                    small: true,
                                    disabled: _busy ||
                                        _stremioEmailCtrl.text.trim().isEmpty ||
                                        _stremioPassCtrl.text.isEmpty,
                                    onTap: () async {
                                      final email = _stremioEmailCtrl.text.trim();
                                      final pass = _stremioPassCtrl.text;
                                      setState(() {
                                        _busy = true;
                                        _problem = null;
                                        _note = null;
                                      });
                                      try {
                                        final res = await app.bridge.addonImportStremio(email, pass);
                                        unawaited(app.loadHome(force: true));
                                        if (!mounted) return;
                                        _stremioEmailCtrl.clear();
                                        _stremioPassCtrl.clear();
                                        final ok = (res['ok'] as num?)?.toInt() ?? 0;
                                        final failed = (res['failed'] as num?)?.toInt() ?? 0;
                                        final msg = failed > 0
                                            ? 'Imported $ok add-on(s); $failed could not be added.'
                                            : (ok == 0 ? 'All your Stremio add-ons are already installed.' : 'Imported $ok add-on(s).');
                                        setState(() => _note = msg);
                                        await _load();
                                      } catch (e) {
                                        if (mounted) setState(() => _problem = e.toString());
                                      } finally {
                                        if (mounted) setState(() => _busy = false);
                                      }
                                    },
                                  ),
                                ],
                              )
                            else
                              Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Wrap(
                                    spacing: 8,
                                    runSpacing: 8,
                                    crossAxisAlignment: WrapCrossAlignment.center,
                                    children: [
                                      ValueListenableBuilder<Color>(
                                        valueListenable: app.lampColorNotifier,
                                        builder: (_, lamp, _) => _SettingsField(
                                          controller: _dirUrlCtrl,
                                          focusNode: _dirUrlFocus,
                                          lamp: lamp,
                                          width: 360,
                                          hintText: 'https://…/addons.json',
                                          onChanged: (_) => setState(() {}),
                                        ),
                                      ),
                                      VeyloPill(
                                        label: _busy ? 'Loading…' : 'Load directory',
                                        icon: 'Globe',
                                        accent: true,
                                        small: true,
                                        disabled: _busy || _dirUrlCtrl.text.trim().isEmpty,
                                        onTap: () async {
                                          final url = _dirUrlCtrl.text.trim();
                                          setState(() {
                                            _busy = true;
                                            _problem = null;
                                            _note = null;
                                          });
                                          try {
                                            final res = await app.bridge.addonFetchDirectory(url);
                                            if (!mounted) return;
                                            setState(() {
                                              _dirEntries = (res['entries'] as List<dynamic>? ?? []).cast<Map<String, dynamic>>();
                                              _dirLoaded = true;
                                            });
                                          } catch (e) {
                                            if (mounted) setState(() => _problem = e.toString());
                                          } finally {
                                            if (mounted) setState(() => _busy = false);
                                          }
                                        },
                                      ),
                                    ],
                                  ),
                                  if (_dirLoaded) ...[
                                    const SizedBox(height: 12),
                                    if (_dirEntries.isEmpty)
                                      Text('No add-ons found in that directory.', style: VeyloText.ui(13.5, weight: 500, color: VeyloColors.dust))
                                    else
                                      ConstrainedBox(
                                        constraints: const BoxConstraints(maxHeight: 260),
                                        child: ListView.separated(
                                          shrinkWrap: true,
                                          itemCount: _dirEntries.length,
                                          separatorBuilder: (_, _) => Container(height: 1, color: VeyloColors.divider),
                                          itemBuilder: (_, idx) {
                                            final entry = _dirEntries[idx];
                                            final installed = entry['installed'] as bool? ?? false;
                                            final url = entry['manifest_url']?.toString() ?? '';
                                            return Padding(
                                              padding: const EdgeInsets.symmetric(vertical: 8),
                                              child: Row(
                                                children: [
                                                  Expanded(
                                                    child: Column(
                                                      crossAxisAlignment: CrossAxisAlignment.start,
                                                      children: [
                                                        Text(entry['name']?.toString() ?? '', style: VeyloText.ui(14, weight: 600)),
                                                        if ((entry['offers']?.toString() ?? '').isNotEmpty)
                                                          Text(entry['offers'].toString(), style: VeyloText.ui(12.5, weight: 400, color: VeyloColors.dust)),
                                                      ],
                                                    ),
                                                  ),
                                                  const SizedBox(width: 10),
                                                  if (installed)
                                                    const _StatusPill(label: 'Installed', icon: 'Check', tone: 'ok')
                                                  else
                                                    VeyloPill(
                                                      label: 'Install',
                                                      icon: 'Plus',
                                                      small: true,
                                                      disabled: _busy,
                                                      onTap: () async {
                                                        setState(() {
                                                          _busy = true;
                                                          _problem = null;
                                                        });
                                                        try {
                                                          final res = await app.bridge.addonInstall(url);
                                                          unawaited(app.loadHome(force: true));
                                                          if (!mounted) return;
                                                          setState(() {
                                                            _dirEntries[idx] = {...entry, 'installed': true};
                                                            _note = 'Installed ${res['name']}.';
                                                          });
                                                          await _load();
                                                        } catch (e) {
                                                          if (mounted) setState(() => _problem = e.toString());
                                                        } finally {
                                                          if (mounted) setState(() => _busy = false);
                                                        }
                                                      },
                                                    ),
                                                ],
                                              ),
                                            );
                                          },
                                        ),
                                      ),
                                  ],
                                ],
                              ),
                          ],
                        ),
                      ),
                      if (_note != null)
                        Container(
                          width: double.infinity,
                          padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 12),
                          color: const Color.fromRGBO(255, 255, 255, 0.05),
                          child: Text(_note!, style: VeyloText.ui(13.5, weight: 500)),
                        ),
                      if (_problem != null)
                        Container(
                          width: double.infinity,
                          padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 12),
                          color: const Color.fromRGBO(255, 110, 95, 0.14),
                          child: Row(
                            children: [
                              const VeyloIcon('Alert', size: 17, color: Color(0xFFFFD2C8)),
                              const SizedBox(width: 10),
                              Expanded(child: Text(_problem!, style: VeyloText.ui(13.5, weight: 500, color: const Color(0xFFFFD2C8)))),
                            ],
                          ),
                        ),
                    ],
                  ),
                ],

                // Display & Discovery
                if (shows(_SettingsCat.display)) ...[
                  const SizedBox(height: 26),
                  const GroupHead(icon: 'Tv', title: 'Display & Discovery'),
                  const SizedBox(height: 10),
                  _SettingsGroup(
                    children: [
                      _SettingRow(
                        name: 'BetterPosters & charts',
                        about: 'Use BetterPosters (btttr.cc) for tagged posters and Trakt/TMDB trending charts.',
                        control: VeyloSegmented<bool>(
                          compact: true,
                          value: _settings['better_posters'] as bool? ?? true,
                          options: const [(true, 'On', null), (false, 'Off', null)],
                          onChanged: (v) => _patch({'better_posters': v}),
                        ),
                      ),
                      _SettingRow(
                        name: 'Hardware video decoding',
                        about: 'Let libmpv use GPU hardware decoding (VA-API / Vulkan / NVDEC) on Linux.',
                        control: VeyloSegmented<String>(
                          compact: true,
                          value: _settings['hwdec_mode']?.toString() ?? 'auto',
                          options: const [('auto', 'Auto', null), ('copy', 'Copy', null), ('no', 'Off', null)],
                          onChanged: (v) => _patch({'hwdec_mode': v}),
                        ),
                      ),
                      _SettingRow(
                        name: 'Hide watched in Picks',
                        about: 'Keep finished films and series out of Tonight\'s Picks duel.',
                        control: VeyloSegmented<bool>(
                          compact: true,
                          value: _settings['picks_hide_watched'] as bool? ?? true,
                          options: const [(true, 'On', null), (false, 'Off', null)],
                          onChanged: (v) => _patch({'picks_hide_watched': v}),
                        ),
                      ),
                      _SettingRow(
                        name: 'Learned taste',
                        about: 'Reset everything Picks has learned from your votes and moods.',
                        control: _armResetTaste
                            ? Row(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  VeyloPill(
                                    label: 'Forget taste',
                                    icon: 'Trash',
                                    confirmRed: true,
                                    small: true,
                                    onTap: () async {
                                      setState(() => _armResetTaste = false);
                                      await app.bridge.picksResetTaste();
                                      app.showToast('Picks taste reset.');
                                    },
                                  ),
                                  const SizedBox(width: 6),
                                  VeyloOrb(icon: 'Close', size: 36, onTap: () => setState(() => _armResetTaste = false)),
                                ],
                              )
                            : VeyloPill(
                                label: 'Start over',
                                icon: 'Undo',
                                small: true,
                                onTap: () => setState(() => _armResetTaste = true),
                              ),
                      ),
                    ],
                  ),
                ],

                // Privacy & About
                if (shows(_SettingsCat.privacy)) ...[
                  const SizedBox(height: 26),
                  const GroupHead(icon: 'Lock', title: 'Privacy & About'),
                  const SizedBox(height: 10),
                  _SettingsGroup(
                    children: [
                      _SettingRow(
                        name: 'Veylo Flutter (Linux preview)',
                        about: 'GPU-rendered Flutter shell over Veylo\'s shared Rust core, SQLite library, Picks AI, and native libmpv.',
                        control: Container(
                          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
                          decoration: BoxDecoration(
                            color: const Color.fromRGBO(46, 204, 113, 0.16),
                            borderRadius: BorderRadius.circular(11),
                          ),
                          child: Text('Rust + Flutter', style: VeyloText.ui(12, weight: 700, color: const Color(0xFF86EFAC))),
                        ),
                      ),
                      _SettingRow(
                        name: 'Opt-in diagnostics',
                        about: _crashReportsAvailable
                            ? 'Off by default. Sends redacted crash and error reports (Sentry) so bugs get fixed. Add-on URLs, API keys, and tokens are never logged or sent.'
                            : 'Off by default. Add-on URLs, API keys, and tokens are never logged or sent. (This build has no crash-report service configured.)',
                        control: VeyloSegmented<bool>(
                          compact: true,
                          value: _settings['telemetry_enabled'] as bool? ?? false,
                          options: const [(true, 'On', null), (false, 'Off', null)],
                          onChanged: (v) => _patch({'telemetry_enabled': v}),
                        ),
                      ),
                      _SettingRow(
                        name: 'Watch history backup',
                        about: 'Export your library & history to JSON, or import from a Veylo / Trakt / Letterboxd export.',
                        control: Wrap(
                          spacing: 8,
                          runSpacing: 8,
                          children: [
                            VeyloPill(
                              label: 'Export JSON',
                              icon: 'Download',
                              small: true,
                              disabled: _busy,
                              onTap: () async {
                                setState(() => _busy = true);
                                try {
                                  final res = await app.bridge.historyExport();
                                  app.showToast('Saved history export to ${res['path']}');
                                } catch (e) {
                                  app.showToast('Export failed: $e', isError: true);
                                } finally {
                                  if (mounted) setState(() => _busy = false);
                                }
                              },
                            ),
                            VeyloPill(
                              label: _histImportOpen ? 'Close' : 'Import',
                              icon: 'Upload',
                              small: true,
                              onTap: () => setState(() => _histImportOpen = !_histImportOpen),
                            ),
                          ],
                        ),
                      ),
                      if (_histImportOpen)
                        Container(
                          width: double.infinity,
                          padding: const EdgeInsets.all(20),
                          color: const Color.fromRGBO(0, 0, 0, 0.22),
                          child: Wrap(
                            spacing: 8,
                            runSpacing: 8,
                            crossAxisAlignment: WrapCrossAlignment.center,
                            children: [
                              ValueListenableBuilder<Color>(
                                valueListenable: app.lampColorNotifier,
                                builder: (_, lamp, _) => _SettingsField(
                                  controller: _histImportCtrl,
                                  focusNode: _histImportFocus,
                                  lamp: lamp,
                                  width: 380,
                                  hintText: 'Paste Veylo JSON, Trakt JSON, or Letterboxd CSV…',
                                  onChanged: (_) => setState(() {}),
                                ),
                              ),
                              VeyloPill(
                                label: 'Import history',
                                icon: 'Check',
                                accent: true,
                                small: true,
                                disabled: _busy || _histImportCtrl.text.trim().isEmpty,
                                onTap: () async {
                                  final raw = _histImportCtrl.text;
                                  setState(() => _busy = true);
                                  try {
                                    final res = await app.bridge.historyImport(raw);
                                    app.bumpPicksDirty();
                                    final n = (res['imported'] as num?)?.toInt() ?? 0;
                                    app.showToast('Imported $n title(s) into Library.');
                                    if (mounted) {
                                      _histImportCtrl.clear();
                                      setState(() => _histImportOpen = false);
                                    }
                                  } catch (e) {
                                    app.showToast('Import failed: $e', isError: true);
                                  } finally {
                                    if (mounted) setState(() => _busy = false);
                                  }
                                },
                              ),
                            ],
                          ),
                        ),
                      _SettingRow(
                        name: 'Diagnostics report',
                        about: 'View or copy a redacted diagnostic snapshot (secrets, add-on URLs, and tokens stripped).',
                        control: Wrap(
                          spacing: 8,
                          children: [
                            VeyloPill(
                              label: _diagReport != null ? 'Hide' : 'View report',
                              icon: 'Info',
                              small: true,
                              onTap: () async {
                                if (_diagReport != null) {
                                  setState(() => _diagReport = null);
                                  return;
                                }
                                try {
                                  final res = await app.bridge.diagnosticReport();
                                  if (mounted) setState(() => _diagReport = res['report']?.toString() ?? '');
                                } catch (e) {
                                  app.showToast('Could not read diagnostics: $e', isError: true);
                                }
                              },
                            ),
                            if (_diagReport != null)
                              VeyloPill(
                                label: 'Copy',
                                icon: 'Copy',
                                small: true,
                                onTap: () async {
                                  await Clipboard.setData(ClipboardData(text: _diagReport!));
                                  app.showToast('Copied redacted diagnostic report.');
                                },
                              ),
                          ],
                        ),
                      ),
                      if (_diagReport != null)
                        Container(
                          width: double.infinity,
                          padding: const EdgeInsets.all(20),
                          color: const Color.fromRGBO(0, 0, 0, 0.28),
                          child: ConstrainedBox(
                            constraints: const BoxConstraints(maxHeight: 220),
                            child: SingleChildScrollView(
                              child: Text(
                                _diagReport!,
                                style: const TextStyle(
                                  fontFamily: 'monospace',
                                  fontSize: 12,
                                  color: VeyloColors.dust,
                                  height: 1.4,
                                ),
                              ),
                            ),
                          ),
                        ),
                    ],
                  ),
                ],
              ],
            ],
          ),
        ),
      ),
    );
  }

  String _bitrateDesc() {
    final cap = (_settings['max_bitrate_mbps'] as num?)?.toInt();
    final mbps = (_settings['connection_mbps'] as num?)?.toDouble();
    if (cap == null) {
      return mbps != null
          ? 'Auto: matches your connection (~${mbps.round()} Mbps measured) so playback won\'t buffer.'
          : 'Auto: measures your connection speed and picks the highest quality that won\'t buffer.';
    }
    if (cap == 0) return 'Unlimited: always picks the highest quality source regardless of connection speed.';
    return 'Prefers sources up to $cap Mbps average bitrate.';
  }
}

class _NavPill extends StatefulWidget {
  const _NavPill({
    required this.icon,
    required this.label,
    required this.active,
    required this.hasDot,
    this.dotColor,
    required this.onTap,
  });
  final String icon;
  final String label;
  final bool active;
  final bool hasDot;
  final Color? dotColor;
  final VoidCallback onTap;

  @override
  State<_NavPill> createState() => _NavPillState();
}

class _NavPillState extends State<_NavPill> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 160),
          height: 40,
          padding: const EdgeInsets.symmetric(horizontal: 16),
          decoration: BoxDecoration(
            color: widget.active
                ? VeyloColors.bone
                : (_hovered ? const Color.fromRGBO(255, 255, 255, 0.11) : const Color.fromRGBO(255, 255, 255, 0.06)),
            borderRadius: BorderRadius.circular(20),
            border: widget.active ? null : Border.all(color: VeyloColors.edge),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              VeyloIcon(widget.icon, size: 16, color: widget.active ? const Color(0xFF000000) : (_hovered ? VeyloColors.bone : VeyloColors.dust)),
              const SizedBox(width: 7),
              Text(
                widget.label,
                style: VeyloText.ui(
                  14,
                  weight: 600,
                  color: widget.active ? const Color(0xFF000000) : (_hovered ? VeyloColors.bone : VeyloColors.dust),
                  height: 1.0,
                ),
              ),
              if (widget.hasDot) ...[
                const SizedBox(width: 6),
                ValueListenableBuilder<Color>(
                  valueListenable: app.lampColorNotifier,
                  builder: (_, lamp, _) => Container(
                    width: 7,
                    height: 7,
                    decoration: BoxDecoration(
                      shape: BoxShape.circle,
                      color: widget.active ? const Color(0xFF000000) : (widget.dotColor ?? lamp),
                    ),
                  ),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

class _SettingsGroup extends StatelessWidget {
  const _SettingsGroup({required this.children});
  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    return GlassBox(
      borderRadius: 22,
      backgroundOverride: const Color.fromRGBO(255, 255, 255, 0.045),
      child: Column(
        children: [
          for (int i = 0; i < children.length; i++) ...[
            if (i > 0) Container(height: 1, color: const Color.fromRGBO(255, 255, 255, 0.075)),
            children[i],
          ],
        ],
      ),
    );
  }
}

class _SettingRow extends StatelessWidget {
  const _SettingRow({required this.name, required this.about, required this.control, this.badge});
  final String name;
  final String about;
  final Widget control;
  final String? badge;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 15),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Wrap(
                  spacing: 8,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    Text(name, style: VeyloText.ui(15, weight: 600)),
                    if (badge != null)
                      Container(
                        padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 2),
                        decoration: BoxDecoration(
                          color: const Color.fromRGBO(255, 255, 255, 0.08),
                          borderRadius: BorderRadius.circular(11),
                        ),
                        child: Text(badge!, style: VeyloText.ui(11.5, weight: 700, color: VeyloColors.dust)),
                      ),
                  ],
                ),
                const SizedBox(height: 3),
                Text(about, style: VeyloText.ui(13.5, weight: 400, height: 1.45, color: VeyloColors.dust)),
              ],
            ),
          ),
          const SizedBox(width: 16),
          control,
        ],
      ),
    );
  }
}

/// Real smoked-glass dropdown menu using OverlayPortal + CompositedTransformFollower.
class _GlassDropdown extends StatefulWidget {
  const _GlassDropdown({required this.value, required this.options, required this.onChanged});
  final String value;
  final List<(String, String)> options;
  final ValueChanged<String> onChanged;

  @override
  State<_GlassDropdown> createState() => _GlassDropdownState();
}

class _GlassDropdownState extends State<_GlassDropdown> {
  final OverlayPortalController _portal = OverlayPortalController();
  final LayerLink _link = LayerLink();
  bool _hovered = false;

  void _toggle() {
    if (_portal.isShowing) {
      _portal.hide();
    } else {
      _portal.show();
    }
    setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final idx = widget.options.indexWhere((o) => o.$1 == widget.value);
    final currentLabel = idx >= 0 ? widget.options[idx].$2 : widget.value;

    return CompositedTransformTarget(
      link: _link,
      child: OverlayPortal(
        controller: _portal,
        overlayChildBuilder: (ctx) => Stack(
          children: [
            Positioned.fill(
              child: GestureDetector(
                behavior: HitTestBehavior.translucent,
                onTap: () {
                  _portal.hide();
                  setState(() {});
                },
              ),
            ),
            CompositedTransformFollower(
              link: _link,
              targetAnchor: Alignment.bottomRight,
              followerAnchor: Alignment.topRight,
              offset: const Offset(0, 6),
              showWhenUnlinked: false,
              child: LiftIn(
                offsetY: -4,
                duration: const Duration(milliseconds: 140),
                child: ConstrainedBox(
                  constraints: const BoxConstraints(minWidth: 210, maxWidth: 260, maxHeight: 320),
                  child: GlassBox(
                    borderRadius: 18,
                    strong: true,
                    padding: const EdgeInsets.all(6),
                    child: SingleChildScrollView(
                      child: ValueListenableBuilder<Color>(
                        valueListenable: app.lampColorNotifier,
                        builder: (_, lamp, _) => Column(
                          mainAxisSize: MainAxisSize.min,
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [
                            for (final (k, label) in widget.options)
                              _DropdownItem(
                                label: label,
                                selected: k == widget.value,
                                lamp: lamp,
                                onTap: () {
                                  _portal.hide();
                                  setState(() {});
                                  widget.onChanged(k);
                                },
                              ),
                          ],
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ],
        ),
        child: MouseRegion(
          cursor: SystemMouseCursors.click,
          onEnter: (_) => setState(() => _hovered = true),
          onExit: (_) => setState(() => _hovered = false),
          child: GestureDetector(
            onTap: _toggle,
            child: AnimatedContainer(
              duration: const Duration(milliseconds: 140),
              height: 40,
              constraints: const BoxConstraints(minWidth: 180),
              padding: const EdgeInsets.fromLTRB(16, 0, 12, 0),
              decoration: BoxDecoration(
                color: (_hovered || _portal.isShowing)
                    ? const Color.fromRGBO(255, 255, 255, 0.13)
                    : const Color.fromRGBO(255, 255, 255, 0.08),
                borderRadius: BorderRadius.circular(20),
                border: Border.all(
                  color: _portal.isShowing ? VeyloColors.sheen : const Color.fromRGBO(255, 255, 255, 0.11),
                ),
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                mainAxisAlignment: MainAxisAlignment.spaceBetween,
                children: [
                  Text(currentLabel, style: VeyloText.ui(14, weight: 600)),
                  const SizedBox(width: 10),
                  const VeyloIcon('Chevron', size: 16, color: VeyloColors.dust),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _DropdownItem extends StatefulWidget {
  const _DropdownItem({required this.label, required this.selected, required this.lamp, required this.onTap});
  final String label;
  final bool selected;
  final Color lamp;
  final VoidCallback onTap;

  @override
  State<_DropdownItem> createState() => _DropdownItemState();
}

class _DropdownItemState extends State<_DropdownItem> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 9),
          decoration: BoxDecoration(
            color: widget.selected
                ? widget.lamp.withValues(alpha: 0.16)
                : (_hovered ? const Color.fromRGBO(255, 255, 255, 0.08) : const Color(0x00000000)),
            borderRadius: BorderRadius.circular(12),
          ),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  widget.label,
                  style: VeyloText.ui(13.5, weight: widget.selected ? 700 : 500, color: VeyloColors.bone),
                ),
              ),
              if (widget.selected) VeyloIcon('Check', size: 15, color: widget.lamp),
            ],
          ),
        ),
      ),
    );
  }
}

class _SettingsField extends StatelessWidget {
  const _SettingsField({
    required this.controller,
    required this.focusNode,
    required this.lamp,
    required this.hintText,
    this.width = 280,
    this.obscureText = false,
    this.onChanged,
    this.onSubmitted,
  });

  final TextEditingController controller;
  final FocusNode focusNode;
  final Color lamp;
  final String hintText;
  final double width;
  final bool obscureText;
  final ValueChanged<String>? onChanged;
  final ValueChanged<String>? onSubmitted;

  @override
  Widget build(BuildContext context) {
    return AnimatedContainer(
      duration: const Duration(milliseconds: 140),
      width: width,
      height: 40,
      padding: const EdgeInsets.symmetric(horizontal: 15),
      alignment: Alignment.centerLeft,
      decoration: BoxDecoration(
        color: const Color.fromRGBO(255, 255, 255, 0.075),
        borderRadius: BorderRadius.circular(20),
        border: Border.all(
          color: focusNode.hasFocus ? lamp : const Color.fromRGBO(255, 255, 255, 0.11),
          width: focusNode.hasFocus ? 1.5 : 1,
        ),
      ),
      child: TextField(
        controller: controller,
        focusNode: focusNode,
        obscureText: obscureText,
        style: VeyloText.ui(14, weight: 500),
        cursorColor: lamp,
        decoration: InputDecoration.collapsed(
          hintText: hintText,
          hintStyle: VeyloText.ui(13.5, weight: 500, color: VeyloColors.dust),
        ),
        onChanged: onChanged,
        onSubmitted: onSubmitted,
      ),
    );
  }
}

class _StatusPill extends StatelessWidget {
  const _StatusPill({required this.label, this.icon, this.tone = 'muted'});
  final String label;
  final String? icon;
  final String tone; // 'ok' | 'warn' | 'muted'

  @override
  Widget build(BuildContext context) {
    final (bg, fg) = switch (tone) {
      'ok' => (const Color.fromRGBO(46, 204, 113, 0.16), const Color(0xFF86EFAC)),
      'warn' => (const Color.fromRGBO(245, 197, 24, 0.18), VeyloColors.amber),
      _ => (const Color.fromRGBO(255, 255, 255, 0.08), VeyloColors.dust),
    };
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 3),
      decoration: BoxDecoration(color: bg, borderRadius: BorderRadius.circular(11)),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (icon != null) ...[
            VeyloIcon(icon!, size: 12.5, color: fg),
            const SizedBox(width: 4),
          ],
          Text(label, style: VeyloText.ui(11.5, weight: 700, color: fg, tabular: true)),
        ],
      ),
    );
  }
}

class _AccountPill extends StatelessWidget {
  const _AccountPill({
    required this.label,
    required this.onTap,
    this.icon,
    this.active = false,
    this.accent = false,
    this.danger = false,
    this.confirmRed = false,
    this.disabled = false,
  });

  final String label;
  final VoidCallback onTap;
  final String? icon;
  final bool active;
  final bool accent;
  final bool danger;
  final bool confirmRed;
  final bool disabled;

  @override
  Widget build(BuildContext context) {
    return FocusableActionDetector(
      enabled: !disabled,
      actions: {
        ActivateIntent: CallbackAction<ActivateIntent>(onInvoke: (_) => onTap()),
      },
      child: Builder(
        builder: (context) => DecoratedBox(
          position: DecorationPosition.foreground,
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(19),
            border: Focus.of(context).hasFocus
                ? Border.all(color: VeyloColors.bone, width: 2)
                : null,
          ),
          child: VeyloPill(
            label: label,
            icon: icon,
            active: active,
            accent: accent,
            danger: danger,
            confirmRed: confirmRed,
            disabled: disabled,
            small: true,
            onTap: onTap,
          ),
        ),
      ),
    );
  }
}

class _AccountBlock extends StatefulWidget {
  const _AccountBlock({required this.initialState, required this.onStateChanged});
  final Map<String, dynamic> initialState;
  final void Function(Map<String, dynamic> state, {bool reloadData}) onStateChanged;

  @override
  State<_AccountBlock> createState() => _AccountBlockState();
}

class _AccountBlockState extends State<_AccountBlock> {
  final TextEditingController _emailCtrl = TextEditingController();
  final TextEditingController _passCtrl = TextEditingController();
  final TextEditingController _keyCtrl = TextEditingController();
  final TextEditingController _linkCtrl = TextEditingController();
  final FocusNode _emailFocus = FocusNode();
  final FocusNode _passFocus = FocusNode();
  final FocusNode _keyFocus = FocusNode();
  final FocusNode _linkFocus = FocusNode();

  late Map<String, dynamic> _state;
  _AccountPanel _panel = _AccountPanel.choices;
  String? _busy;
  String? _problem;
  String? _note;
  bool _showKey = false;
  bool _enterKeyOpen = false;
  bool _confirmDelete = false;
  bool? _keyCopied;
  Timer? _copyTimer;
  ({String code, String checkCode})? _checkedLink;
  AppState? _pollingApp;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final app = AppStateScope.read(context);
    if (_pollingApp == app) return;
    _pollingApp?.setFastSyncPolling(false);
    _pollingApp = app..setFastSyncPolling(true);
  }

  @override
  void initState() {
    super.initState();
    _state = widget.initialState;
    _enterKeyOpen = _state['sync_key_needed'] as bool? ?? false;
    _emailFocus.addListener(_onFocus);
    _passFocus.addListener(_onFocus);
    _keyFocus.addListener(_onFocus);
    _linkFocus.addListener(_onFocus);
  }

  @override
  void didUpdateWidget(covariant _AccountBlock oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.initialState != oldWidget.initialState) {
      final oldUid = (oldWidget.initialState['account'] as Map<String, dynamic>?)?['uid'];
      final newUid = (widget.initialState['account'] as Map<String, dynamic>?)?['uid'];
      _state = widget.initialState;
      if (oldUid != newUid) {
        _showKey = false;
        _confirmDelete = false;
        _keyCopied = null;
        _passCtrl.clear();
        _keyCtrl.clear();
        _linkCtrl.clear();
        _checkedLink = null;
        _panel = _AccountPanel.choices;
        _enterKeyOpen = _state['sync_key_needed'] as bool? ?? false;
      }
    }
  }

  void _onFocus() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _copyTimer?.cancel();
    _pollingApp?.setFastSyncPolling(false);
    _emailFocus.removeListener(_onFocus);
    _passFocus.removeListener(_onFocus);
    _keyFocus.removeListener(_onFocus);
    _linkFocus.removeListener(_onFocus);
    _emailCtrl.dispose();
    _passCtrl.dispose();
    _keyCtrl.dispose();
    _linkCtrl.dispose();
    _emailFocus.dispose();
    _passFocus.dispose();
    _keyFocus.dispose();
    _linkFocus.dispose();
    super.dispose();
  }

  Future<void> _run(String tag, Future<(Map<String, dynamic>?, String?)> Function() job, {bool reloadData = false}) async {
    if (_busy != null) return;
    setState(() {
      _busy = tag;
      _problem = null;
      _note = null;
    });
    try {
      final (nextState, note) = await job();
      if (!mounted) return;
      if (nextState != null) {
        final keyNeeded = nextState['sync_key_needed'] as bool? ?? false;
        setState(() {
          _state = nextState;
          _note = note;
          if (nextState['account'] != null) {
            _panel = _AccountPanel.choices;
            _passCtrl.clear();
          }
          if (keyNeeded) _enterKeyOpen = true;
        });
        widget.onStateChanged(nextState, reloadData: reloadData);
      } else {
        setState(() => _note = note);
      }
    } on PlatformException catch (e) {
      if (mounted && e.code != 'cancel') {
        setState(() => _problem = e.message ?? 'The device could not complete the request.');
      }
    } catch (e) {
      if (mounted) {
        final msg = e.toString().replaceFirst(RegExp(r'^Exception:\s*'), '');
        setState(() => _problem = msg);
      }
    } finally {
      if (mounted) setState(() => _busy = null);
    }
  }

  String _syncedLabel() {
    final at = (_state['last_synced'] as num?)?.toInt() ?? 0;
    if (at <= 0) return 'Not synced yet';
    final now = (_state['now_secs'] as num?)?.toInt() ?? (DateTime.now().millisecondsSinceEpoch ~/ 1000);
    final ago = (now - at).clamp(0, 999999999);
    if (ago < 60) return 'Synced just now';
    if (ago < 3600) return 'Synced ${ago ~/ 60} min ago';
    return 'Synced ${ago ~/ 3600} h ago';
  }

  void _submitSignIn() {
    final email = _emailCtrl.text.trim();
    final pass = _passCtrl.text;
    if (email.isEmpty || pass.isEmpty) return;
    final app = AppStateScope.read(context);
    unawaited(_run('in', () async {
      final s = await app.bridge.accountSignIn(email, pass);
      return (s, null);
    }, reloadData: true));
  }

  @override
  Widget build(BuildContext context) {
    final app = AppStateScope.read(context);
    final account = _state['account'] as Map<String, dynamic>?;
    final accountsAvailable =
        _state['accounts_available'] as bool? ?? app.accountsAvailable;
    final googleAvailable =
        _state['google_available'] as bool? ?? app.googleAvailable;
    final keyNeeded = _state['sync_key_needed'] as bool? ?? false;
    final recoveryCode = keyNeeded ? null : _state['recovery_code'] as String?;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const GroupHead(icon: 'Users', title: 'Account & Sync'),
        const SizedBox(height: 10),
        _SettingsGroup(
          children: [
            if (!accountsAvailable)
              Align(
                alignment: Alignment.centerLeft,
                child: Padding(
                  padding: const EdgeInsets.all(20),
                  child: _googleNotConfigured(),
                ),
              )
            else if (account != null) ...[
              // Signed-in hero row
              _buildSignedInHero(app, account, keyNeeded),
              // E2EE Recovery Key row
              _SettingRow(
                name: 'End-to-end encryption key',
                about: keyNeeded
                    ? 'This account has encrypted cloud data from another device. Link via QR or enter its Recovery Key to unlock sync.'
                    : 'Library, progress, likes, preferences and add-ons are AES-256-GCM encrypted on-device. Keep a backup of your Recovery Key for new devices.',
                control: Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    if (recoveryCode != null) ...[
                      _AccountPill(
                        label: _showKey ? 'Hide' : 'Reveal',
                        onTap: () => setState(() => _showKey = !_showKey),
                      ),
                      _AccountPill(
                        label: switch (_keyCopied) {
                          true => 'Copied',
                          false => "Couldn't copy",
                          null => 'Copy',
                        },
                        icon: 'Copy',
                        onTap: () async {
                          try {
                            await Clipboard.setData(ClipboardData(text: recoveryCode));
                            if (mounted) setState(() => _keyCopied = true);
                          } catch (_) {
                            if (mounted) setState(() => _keyCopied = false);
                          }
                          _copyTimer?.cancel();
                          _copyTimer = Timer(const Duration(milliseconds: 2200), () {
                            if (mounted) setState(() => _keyCopied = null);
                          });
                        },
                      ),
                    ],
                    _AccountPill(
                      label: _enterKeyOpen ? 'Close' : 'Enter key',
                      accent: keyNeeded && !_enterKeyOpen,
                      onTap: () => setState(() => _enterKeyOpen = !_enterKeyOpen),
                    ),
                  ],
                ),
              ),
              if (_showKey && recoveryCode != null)
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.all(20),
                  color: const Color.fromRGBO(0, 0, 0, 0.22),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        'Your 256-bit Sync Recovery Key (never shared with Firebase):',
                        style: VeyloText.ui(13.5, weight: 500, color: VeyloColors.dust),
                      ),
                      const SizedBox(height: 10),
                      Container(
                        width: double.infinity,
                        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
                        decoration: BoxDecoration(
                          color: const Color.fromRGBO(0, 0, 0, 0.45),
                          borderRadius: BorderRadius.circular(14),
                          border: Border.all(color: VeyloColors.edge),
                        ),
                        child: Text(
                          recoveryCode,
                          style: const TextStyle(
                            fontFamily: 'monospace',
                            fontSize: 13.5,
                            fontWeight: FontWeight.w600,
                            color: VeyloColors.bone,
                            letterSpacing: 0.3,
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              if (_enterKeyOpen || keyNeeded)
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.all(20),
                  color: const Color.fromRGBO(0, 0, 0, 0.22),
                  child: Wrap(
                    spacing: 8,
                    runSpacing: 8,
                    crossAxisAlignment: WrapCrossAlignment.center,
                    children: [
                      ValueListenableBuilder<Color>(
                        valueListenable: app.lampColorNotifier,
                        builder: (_, lamp, _) => _SettingsField(
                          controller: _keyCtrl,
                          focusNode: _keyFocus,
                          lamp: lamp,
                          width: 360,
                          obscureText: true,
                          hintText: 'VEYLO-… recovery key',
                          onChanged: (_) => setState(() {}),
                        ),
                      ),
                      _AccountPill(
                        label: _busy == 'unlock' ? 'Unlocking…' : 'Unlock & sync',
                        icon: 'Unlock',
                        accent: true,
                        disabled: _busy != null || _keyCtrl.text.trim().isEmpty,
                        onTap: () {
                          final code = _keyCtrl.text.trim();
                          unawaited(_run('unlock', () async {
                            final s = await app.bridge.accountSetRecoveryCode(code);
                            if (mounted) {
                              _keyCtrl.clear();
                              setState(() {
                                _showKey = false;
                                _enterKeyOpen = false;
                              });
                            }
                            final took = (s['took'] as num?)?.toInt() ?? 0;
                            final sent = (s['sent'] as num?)?.toInt() ?? 0;
                            return (s, 'Recovery key applied ($took in, $sent out).');
                          }, reloadData: true));
                        },
                      ),
                    ],
                  ),
                ),
              _buildLinkSender(app, keyNeeded: keyNeeded),
              // Session & cloud data row
              _SettingRow(
                name: 'Session & cloud data',
                about: 'Signing out keeps your local library. Deleting removes your account and all encrypted cloud data.',
                control: Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    _AccountPill(
                      label: 'Sign out',
                      disabled: _busy != null,
                      onTap: () => _run('out', () async {
                        final s = await app.bridge.accountSignOut();
                        if (mounted) {
                          setState(() {
                            _showKey = false;
                            _confirmDelete = false;
                            _enterKeyOpen = false;
                            _keyCopied = null;
                          });
                        }
                        return (s, null);
                      }),
                    ),
                    if (_confirmDelete)
                      Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          _AccountPill(
                            label: _busy == 'delete' ? 'Deleting…' : 'Confirm delete',
                            icon: 'Trash',
                            confirmRed: true,
                            disabled: _busy != null,
                            onTap: () {
                              setState(() => _confirmDelete = false);
                              unawaited(_run('delete', () async {
                                final s = await app.bridge.accountDelete();
                                return (s, 'Account deleted.');
                              }));
                            },
                          ),
                          const SizedBox(width: 6),
                          VeyloOrb(icon: 'Close', size: 36, onTap: () => setState(() => _confirmDelete = false)),
                        ],
                      )
                    else
                      _AccountPill(
                        label: 'Delete account',
                        icon: 'Trash',
                        danger: true,
                        disabled: _busy != null,
                        onTap: () => setState(() => _confirmDelete = true),
                      ),
                  ],
                ),
              ),
            ] else ...[
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 16),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Sign in to end-to-end encrypt and sync your library, progress, likes, preferences and add-ons across devices.',
                      style: VeyloText.ui(13.5, weight: 400, height: 1.45, color: VeyloColors.dust),
                    ),
                    if (!googleAvailable) ...[
                      const SizedBox(height: 8),
                      _googleNotConfigured(),
                    ],
                    const SizedBox(height: 12),
                    Wrap(
                      spacing: 8,
                      runSpacing: 8,
                      children: [
                        if (googleAvailable)
                          _AccountPill(
                            label: _busy == 'google'
                                ? (Platform.isAndroid ? 'Signing in…' : 'Finish in browser…')
                                : 'Sign in with Google',
                            icon: 'Globe',
                            accent: true,
                            disabled: _busy != null,
                            onTap: () => _run('google', () async {
                              if (_panel == _AccountPanel.qr) {
                                setState(() => _panel = _AccountPanel.choices);
                              }
                              final state = await app.bridge.accountSignInGoogle();
                              return (state, state == null ? null : 'Signed in with Google.');
                            }, reloadData: true),
                          ),
                        _AccountPill(
                          label: 'Email',
                          icon: 'Pen',
                          active: _panel == _AccountPanel.email,
                          disabled: _busy != null,
                          onTap: () => _togglePanel(_AccountPanel.email),
                        ),
                        _AccountPill(
                          label: 'Sign in with a code',
                          icon: 'Grid',
                          active: _panel == _AccountPanel.qr,
                          disabled: _busy != null,
                          onTap: () => _togglePanel(_AccountPanel.qr),
                        ),
                      ],
                    ),
                  ],
                ),
              ),
              if (_panel == _AccountPanel.email)
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.all(20),
                  color: const Color.fromRGBO(0, 0, 0, 0.22),
                  child: ValueListenableBuilder<Color>(
                    valueListenable: app.lampColorNotifier,
                    builder: (_, lamp, _) {
                      final credsEmpty = _emailCtrl.text.trim().isEmpty || _passCtrl.text.isEmpty;
                      return Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Wrap(
                            spacing: 10,
                            runSpacing: 10,
                            children: [
                              _SettingsField(
                                controller: _emailCtrl,
                                focusNode: _emailFocus,
                                lamp: lamp,
                                width: 280,
                                hintText: 'Email',
                                onChanged: (_) => setState(() {}),
                                onSubmitted: (_) => _submitSignIn(),
                              ),
                              _SettingsField(
                                controller: _passCtrl,
                                focusNode: _passFocus,
                                lamp: lamp,
                                width: 260,
                                obscureText: true,
                                hintText: 'Password',
                                onChanged: (_) => setState(() {}),
                                onSubmitted: (_) => _submitSignIn(),
                              ),
                            ],
                          ),
                          const SizedBox(height: 12),
                          Wrap(
                            spacing: 8,
                            runSpacing: 8,
                            crossAxisAlignment: WrapCrossAlignment.center,
                            children: [
                              _AccountPill(
                                label: _busy == 'in' ? 'Signing in…' : 'Sign in',
                                accent: true,
                                disabled: _busy != null || credsEmpty,
                                onTap: _submitSignIn,
                              ),
                              _AccountPill(
                                label: _busy == 'up' ? 'Creating…' : 'Create account',
                                disabled: _busy != null || credsEmpty,
                                onTap: () {
                                  final email = _emailCtrl.text.trim();
                                  final pass = _passCtrl.text;
                                  unawaited(_run('up', () async {
                                    final s = await app.bridge.accountSignUp(email, pass);
                                    return (s, 'Account created.');
                                  }, reloadData: true));
                                },
                              ),
                              MouseRegion(
                                cursor: SystemMouseCursors.click,
                                child: GestureDetector(
                                  onTap: _busy != null
                                      ? null
                                      : () {
                                          final email = _emailCtrl.text.trim();
                                          if (email.isEmpty) {
                                            setState(() => _problem = 'Type your email first.');
                                            return;
                                          }
                                          unawaited(_run('reset', () async {
                                            await app.bridge.accountResetPassword(email);
                                            return (null, 'Check your email for a link to choose a new password.');
                                          }));
                                        },
                                  child: Padding(
                                    padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
                                    child: Text(
                                      'Forgot password?',
                                      style: VeyloText.ui(13.5, weight: 600, color: VeyloColors.dust),
                                    ),
                                  ),
                                ),
                              ),
                            ],
                          ),
                        ],
                      );
                    },
                  ),
                ),
              if (_panel == _AccountPanel.qr)
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.all(20),
                  color: const Color.fromRGBO(0, 0, 0, 0.22),
                  child: _QrLinkPanel(
                    onSignedIn: (nextState) {
                      if (!mounted) return;
                      setState(() {
                        _state = nextState;
                        _panel = _AccountPanel.choices;
                        _note = 'Device linked and signed in.';
                        _problem = null;
                      });
                      widget.onStateChanged(nextState, reloadData: true);
                    },
                  ),
                ),
            ],
            if (_problem != null)
              Container(
                width: double.infinity,
                padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 12),
                color: const Color.fromRGBO(255, 110, 95, 0.14),
                child: Row(
                  children: [
                    const VeyloIcon('Alert', size: 17, color: Color(0xFFFFD2C8)),
                    const SizedBox(width: 10),
                    Expanded(child: Text(_problem!, style: VeyloText.ui(13.5, weight: 500, color: const Color(0xFFFFD2C8)))),
                  ],
                ),
              ),
            if (_note != null)
              Container(
                width: double.infinity,
                padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 12),
                color: const Color.fromRGBO(46, 204, 113, 0.11),
                child: Row(
                  children: [
                    const VeyloIcon('Check', size: 17, color: Color(0xFF86EFAC)),
                    const SizedBox(width: 10),
                    Expanded(child: Text(_note!, style: VeyloText.ui(13.5, weight: 500, color: VeyloColors.bone))),
                  ],
                ),
              ),
          ],
        ),
      ],
    );
  }

  Widget _googleNotConfigured() => Text(
    "Google sign-in isn't set up in this build.",
    style: VeyloText.ui(13.5, weight: 400, color: VeyloColors.dust),
  );

  void _togglePanel(_AccountPanel panel) {
    setState(() {
      _panel = _panel == panel ? _AccountPanel.choices : panel;
      _problem = null;
      _note = null;
      if (panel == _AccountPanel.linkSender) {
        _linkCtrl.clear();
        _checkedLink = null;
      }
    });
  }

  Future<void> _checkLinkCode(VeyloBridge bridge, String code) async {
    final checkCode = await bridge.accountCheckLink(code);
    if (mounted && _linkCtrl.text.trim() == code) {
      setState(() => _checkedLink = (code: code, checkCode: checkCode));
    }
  }

  Widget _buildLinkSender(AppState app, {required bool keyNeeded}) {
    final open = !keyNeeded && _panel == _AccountPanel.linkSender;
    final checked = _checkedLink;
    return SizedBox(
      width: double.infinity,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 15),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _AccountPill(
              label: 'Sign in another device',
              active: open,
              disabled: _busy != null || keyNeeded,
              onTap: () => _togglePanel(_AccountPanel.linkSender),
            ),
            if (keyNeeded) ...[
              const SizedBox(height: 8),
              Text(
                'Enter your Recovery Key to sign in another device.',
                style: VeyloText.ui(13.5, weight: 400, color: VeyloColors.dust),
              ),
            ],
            if (open) ...[
              const SizedBox(height: 10),
              Text(
                checked != null
                    ? 'Confirm that the other device shows ${checked.checkCode}. This shares your session and encryption key.'
                    : Platform.isAndroid
                    ? 'Enter or scan the sign-in code shown on the other device.'
                    : 'Enter the sign-in code shown on the other device.',
                style: VeyloText.ui(13.5, weight: 400, height: 1.45, color: VeyloColors.dust),
              ),
              const SizedBox(height: 8),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  ValueListenableBuilder<Color>(
                    valueListenable: app.lampColorNotifier,
                    builder: (_, lamp, _) => _SettingsField(
                      controller: _linkCtrl,
                      focusNode: _linkFocus,
                      lamp: lamp,
                      hintText: 'Sign-in code',
                      obscureText: true,
                      onChanged: (_) => setState(() => _checkedLink = null),
                    ),
                  ),
                  if (Platform.isAndroid)
                    _AccountPill(
                      label: _busy == 'scan' ? 'Scanning…' : 'Scan code',
                      icon: 'Grid',
                      disabled: _busy != null,
                      onTap: () => _run('scan', () async {
                        final scanned = await app.bridge.accountScanCode();
                        if (!mounted || scanned == null) return (null, null);
                        final code = scanned.trim();
                        setState(() {
                          _linkCtrl.text = code;
                          _checkedLink = null;
                        });
                        await _checkLinkCode(app.bridge, code);
                        return (null, null);
                      }),
                    ),
                  if (checked == null)
                    _AccountPill(
                      label: _busy == 'checkLink' ? 'Checking…' : 'Check code',
                      disabled: _busy != null || _linkCtrl.text.trim().isEmpty,
                      onTap: () => _run('checkLink', () async {
                        await _checkLinkCode(app.bridge, _linkCtrl.text.trim());
                        return (null, null);
                      }),
                    )
                  else
                    _AccountPill(
                      label: _busy == 'answerLink' ? 'Confirming…' : 'Confirm',
                      icon: 'Check',
                      accent: true,
                      disabled: _busy != null,
                      onTap: () => _run('answerLink', () async {
                        await app.bridge.accountAnswerLink(checked.code);
                        if (mounted) _togglePanel(_AccountPanel.linkSender);
                        return (null, 'Sign-in approved. Check the other device.');
                      }),
                    ),
                ],
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _buildSignedInHero(AppState app, Map<String, dynamic> account, bool keyNeeded) {
    final name = account['name']?.toString() ?? '';
    final email = account['email']?.toString() ?? '';
    final provider = account['provider']?.toString() ?? '';
    final display = name.isNotEmpty ? name : (email.isNotEmpty ? email : 'Signed in');
    final initial = display.isNotEmpty ? display[0].toUpperCase() : 'V';
    final subParts = <String>[
      if (email.isNotEmpty && name.isNotEmpty) email,
      if (provider == 'google') 'Google',
      _syncedLabel(),
    ];

    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 16),
      child: Row(
        children: [
          ValueListenableBuilder<Color>(
            valueListenable: app.lampColorNotifier,
            builder: (_, lamp, _) => Container(
              width: 44,
              height: 44,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: lamp.withValues(alpha: 0.22),
                border: Border.all(color: lamp.withValues(alpha: 0.55)),
              ),
              child: Text(initial, style: VeyloText.display(19, weight: 700, color: VeyloColors.bone)),
            ),
          ),
          const SizedBox(width: 14),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Wrap(
                  spacing: 8,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    Text(display, style: VeyloText.ui(15.5, weight: 650)),
                    if (keyNeeded)
                      const _StatusPill(label: 'Key needed', icon: 'Alert', tone: 'warn')
                    else
                      const _StatusPill(label: 'E2EE active', icon: 'Lock', tone: 'ok'),
                  ],
                ),
                const SizedBox(height: 3),
                Text(
                  subParts.join(' · '),
                  style: VeyloText.ui(13.5, weight: 400, color: VeyloColors.dust, tabular: true),
                ),
              ],
            ),
          ),
          const SizedBox(width: 14),
          _AccountPill(
            label: _busy == 'sync' ? 'Syncing…' : 'Sync now',
            icon: 'Refresh',
            disabled: _busy != null,
            onTap: () => _run('sync', () async {
              final s = await app.bridge.accountSyncNow();
              final took = (s['took'] as num?)?.toInt() ?? 0;
              final sent = (s['sent'] as num?)?.toInt() ?? 0;
              final msg = (took + sent == 0) ? 'Everything is up to date.' : 'Synced: $took in, $sent out.';
              return (s, msg);
            }, reloadData: true),
          ),
        ],
      ),
    );
  }
}

class _QrLinkPanel extends StatefulWidget {
  const _QrLinkPanel({required this.onSignedIn});
  final ValueChanged<Map<String, dynamic>> onSignedIn;

  @override
  State<_QrLinkPanel> createState() => _QrLinkPanelState();
}

class _QrLinkPanelState extends State<_QrLinkPanel> {
  final VeyloBridge _bridge = VeyloBridge.instance;
  int? _offerId;
  String? _svg;
  String? _code;
  String? _checkCode;
  String? _problem;
  bool _loading = true;
  bool _showCode = false;
  int _round = 0;

  @override
  void initState() {
    super.initState();
    unawaited(_startOffer());
  }

  @override
  void dispose() {
    _cancelOffer(_offerId);
    super.dispose();
  }

  void _cancelOffer(int? id) {
    if (id != null) {
      unawaited(_bridge.accountCancelLink(offerId: id).catchError((_) {}));
    }
  }

  Future<void> _startOffer() async {
    final myRound = ++_round;
    setState(() {
      _loading = true;
      _offerId = null;
      _svg = null;
      _code = null;
      _checkCode = null;
      _problem = null;
      _showCode = false;
    });
    late final int offerId;
    try {
      final res = await _bridge.accountOfferLink();
      final id = (res['offer_id'] as num?)?.toInt();
      if (!mounted || myRound != _round) {
        _cancelOffer(id);
        return;
      }
      if (id == null) {
        setState(() {
          _loading = false;
          _problem = "Couldn't create a sign-in code. Try a new code.";
        });
        return;
      }
      offerId = id;
      setState(() {
        _loading = false;
        _offerId = id;
        _svg = res['svg'] as String?;
        _code = res['code'] as String?;
        _checkCode = res['check_code'] as String?;
      });
    } catch (e) {
      if (!mounted || myRound != _round) return;
      setState(() {
        _loading = false;
        _problem = e.toString().replaceFirst(RegExp(r'^Exception:\s*'), '');
      });
      return;
    }

    final elapsed = Stopwatch()..start();
    while (elapsed.elapsed < const Duration(minutes: 10)) {
      await Future<void>.delayed(const Duration(seconds: 2));
      if (!mounted || myRound != _round) return;
      try {
        final poll = await _bridge.accountTakeLink(offerId: offerId);
        if (!mounted || myRound != _round) return;
        if (poll['signed_in'] == true && poll['state'] is Map<String, dynamic>) {
          _offerId = null;
          widget.onSignedIn(poll['state'] as Map<String, dynamic>);
          return;
        }
        if (poll['expired'] == true) break;
        final error = poll['error'] as String?;
        if (_problem != error) setState(() => _problem = error);
        if (error != null && poll['transient'] != true) return;
      } catch (e) {
        if (!mounted || myRound != _round) return;
        setState(() {
          _problem = e.toString().replaceFirst(RegExp(r'^Exception:\s*'), '');
        });
        return;
      }
    }
    if (mounted && myRound == _round) unawaited(_startOffer());
  }

  @override
  Widget build(BuildContext context) {
    final svg = _svg;
    final code = _code;
    final qr = svg == null
        ? null
        : Container(
            width: 176,
            height: 176,
            padding: const EdgeInsets.all(8),
            decoration: BoxDecoration(
              color: const Color(0xFFFFFFFF),
              borderRadius: BorderRadius.circular(16),
            ),
            child: SvgPicture.string(svg, fit: BoxFit.contain),
          );
    final details = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          'On a signed-in device, open Settings → Account → Sign in another device. Enter or scan this code and confirm the verification code matches.',
          style: VeyloText.ui(13.5, weight: 400, height: 1.45, color: VeyloColors.dust),
        ),
        if (_checkCode != null) ...[
          const SizedBox(height: 10),
          _StatusPill(
            label: 'Verification code: $_checkCode',
            icon: 'Check',
            tone: 'ok',
          ),
        ],
        if (code != null && _showCode) ...[
          const SizedBox(height: 10),
          Text(code, style: VeyloText.ui(13.5, weight: 500)),
        ],
        if (_problem != null) ...[
          const SizedBox(height: 10),
          Text(
            _problem!,
            style: VeyloText.ui(13, weight: 500, color: const Color(0xFFFFD2C8)),
          ),
        ],
        const SizedBox(height: 14),
        Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [
            if (code != null)
              _AccountPill(
                label: _showCode ? 'Hide sign-in code' : 'Show sign-in code',
                onTap: () => setState(() => _showCode = !_showCode),
              ),
            if (code != null && _showCode)
              _AccountPill(
                label: 'Copy code',
                onTap: () async {
                  try {
                    await Clipboard.setData(ClipboardData(text: code));
                    if (context.mounted) {
                      AppStateScope.read(context).showToast('Sign-in code copied.');
                    }
                  } catch (_) {
                    if (context.mounted) {
                      AppStateScope.read(context).showToast(
                        "Couldn't copy the sign-in code.",
                        isError: true,
                      );
                    }
                  }
                },
              ),
            _AccountPill(
              label: _loading ? 'Generating…' : 'New code',
              icon: 'Refresh',
              disabled: _loading,
              onTap: () => unawaited(_startOffer()),
            ),
          ],
        ),
      ],
    );
    if (qr == null) return details;
    return LayoutBuilder(
      builder: (context, constraints) {
        if (constraints.maxWidth < 440) {
          return Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [qr, const SizedBox(height: 16), details],
          );
        }
        return Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [qr, const SizedBox(width: 20), Expanded(child: details)],
        );
      },
    );
  }
}

class _AddonRow extends StatelessWidget {
  const _AddonRow({
    required this.addon,
    required this.busy,
    required this.armed,
    required this.onArm,
    required this.onCancelArm,
    required this.onConfigure,
    required this.onUpdate,
    required this.onRemove,
  });

  final Map<String, dynamic> addon;
  final bool busy;
  final bool armed;
  final VoidCallback onArm;
  final VoidCallback onCancelArm;
  final VoidCallback onConfigure;
  final VoidCallback onUpdate;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) {
    final name = addon['name']?.toString() ?? '';
    final ver = addon['version']?.toString() ?? '';
    final offers = addon['offers']?.toString() ?? '';
    final update = addon['update'] as String?;
    final configurable = addon['configurable'] as bool? ?? false;
    final removable = addon['removable'] as bool? ?? false;

    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 14),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Wrap(
                  spacing: 8,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    Text(name, style: VeyloText.ui(15, weight: 600)),
                    if (ver.isNotEmpty)
                      Container(
                        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
                        decoration: BoxDecoration(color: const Color.fromRGBO(255, 255, 255, 0.08), borderRadius: BorderRadius.circular(10)),
                        child: Text('v$ver', style: VeyloText.ui(11.5, weight: 600, color: VeyloColors.dust, tabular: true)),
                      ),
                    if (!removable)
                      Container(
                        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
                        decoration: BoxDecoration(color: const Color.fromRGBO(255, 255, 255, 0.08), borderRadius: BorderRadius.circular(10)),
                        child: Text('Core', style: VeyloText.ui(11.5, weight: 600, color: VeyloColors.dust)),
                      ),
                  ],
                ),
                if (offers.isNotEmpty) ...[
                  const SizedBox(height: 2),
                  Text(offers, style: VeyloText.ui(13, weight: 400, color: VeyloColors.dust)),
                ],
              ],
            ),
          ),
          Wrap(
            spacing: 8,
            children: [
              if (update != null)
                VeyloPill(label: 'Update $update', icon: 'Refresh', accent: true, small: true, disabled: busy, onTap: onUpdate),
              if (configurable)
                VeyloPill(label: 'Configure', icon: 'Sliders', small: true, disabled: busy, onTap: onConfigure),
              if (removable)
                armed
                    ? Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          VeyloPill(label: 'Confirm', icon: 'Trash', confirmRed: true, small: true, disabled: busy, onTap: onRemove),
                          const SizedBox(width: 6),
                          VeyloOrb(icon: 'Close', size: 36, onTap: onCancelArm),
                        ],
                      )
                    : VeyloPill(label: 'Remove', icon: 'Trash', danger: true, small: true, disabled: busy, onTap: onArm),
            ],
          ),
        ],
      ),
    );
  }
}
