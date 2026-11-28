import 'dart:async';

import 'package:cached_network_image/cached_network_image.dart';
import 'package:flutter/widgets.dart';

import '../bridge/veylo_bridge.dart';

/// The width the title page decodes its hero backdrop at: the physical width it covers,
/// bounded (sharp on large screens, no huge decodes).
int heroDecodeWidth(double logicalWidth, double dpr) => (logicalWidth * dpr).clamp(960, 2880).round();

/// The exact image providers the title page paints first (same URL + decode size = same
/// ImageCache entry), shared by the page's reveal and the card-hover prefetch.
ImageProvider heroProvider(String url, int decodeWidth) =>
    ResizeImage.resizeIfNeeded(decodeWidth, null, CachedNetworkImageProvider(url));
ImageProvider logoProvider(String url, {int decodeWidth = 920}) =>
    ResizeImage.resizeIfNeeded(decodeWidth, null, CachedNetworkImageProvider(url));

Future<bool> precacheArtwork(ImageProvider provider, BuildContext context) async {
  var loaded = true;
  await precacheImage(provider, context, onError: (_, _) => loaded = false);
  return loaded;
}

final Set<String> _artWarmed = <String>{};

/// Card hover/focus: warm the title page's data (Rust caches) and download + decode its hero
/// backdrop and logo, so opening it paints from memory.
void prefetchTitle(BuildContext context, MediaItem media) {
  final bridge = VeyloBridge.instance;
  bridge.titlePrefetch(media);
  if (!_artWarmed.add(media.key)) return;
  if (_artWarmed.length > 200) _artWarmed.remove(_artWarmed.first);
  final width = heroDecodeWidth(MediaQuery.sizeOf(context).width, MediaQuery.devicePixelRatioOf(context));
  unawaited(() async {
    try {
      final art = await bridge.call('title_prefetch_art', {'media': media.toArg()});
      final backdrop = art['backdrop'] as String?;
      final logo = art['logo'] as String?;
      if (backdrop != null && backdrop.isNotEmpty) _warm(heroProvider(backdrop, width));
      if (logo != null && logo.isNotEmpty) _warm(logoProvider(logo));
    } catch (_) {
      _artWarmed.remove(media.key);
    }
  }());
}

/// Like precacheImage, without needing a mounted context.
void _warm(ImageProvider provider) {
  final stream = provider.resolve(ImageConfiguration.empty);
  late final ImageStreamListener listener;
  listener = ImageStreamListener(
    (_, _) => stream.removeListener(listener),
    onError: (_, _) => stream.removeListener(listener),
  );
  stream.addListener(listener);
}
