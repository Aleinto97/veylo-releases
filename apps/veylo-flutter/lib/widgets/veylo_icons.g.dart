// GENERATED CODE - DO NOT MODIFY BY HAND.
// Generated from apps/veylo-dx/src/icons.rs by tool/gen_icons.py.

sealed class VeyloIconPrim {
  const VeyloIconPrim();
}

final class VeyloIconPath extends VeyloIconPrim {
  const VeyloIconPath(this.d);
  final String d;
}

final class VeyloIconCircle extends VeyloIconPrim {
  const VeyloIconCircle(this.cx, this.cy, this.r);
  final double cx;
  final double cy;
  final double r;
}

final class VeyloIconRect extends VeyloIconPrim {
  const VeyloIconRect(this.x, this.y, this.w, this.h, this.rx, this.ry);
  final double x;
  final double y;
  final double w;
  final double h;
  final double rx;
  final double ry;
}

const Set<String> kVeyloFillableIcons = {
  'ThumbUp',
  'ThumbDown',
  'Play',
  'Sparkles',
  'Star',
  'Heart',
  'Bookmark',
  'Next',
  'Pause',
  'Moon',
};

const Map<String, List<VeyloIconPrim>> kVeyloIcons = {
  'Search': [VeyloIconPath('m21 21l-4.34-4.34'), VeyloIconCircle(11.0, 11.0, 8.0)],
  'Close': [VeyloIconPath('M18 6L6 18M6 6l12 12')],
  'Download': [VeyloIconPath('M12 15V3m9 12v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4'), VeyloIconPath('m7 10l5 5l5-5')],
  'History': [VeyloIconPath('M3 12a9 9 0 1 0 9-9a9.75 9.75 0 0 0-6.74 2.74L3 8'), VeyloIconPath('M3 3v5h5m4-1v5l4 2')],
  'ThumbUp': [VeyloIconPath('M15 5.88L14 10h5.83a2 2 0 0 1 1.92 2.56l-2.33 8A2 2 0 0 1 17.5 22H4a2 2 0 0 1-2-2v-8a2 2 0 0 1 2-2h2.76a2 2 0 0 0 1.79-1.11L12 2a3.13 3.13 0 0 1 3 3.88M7 10v12')],
  'ThumbDown': [VeyloIconPath('M9 18.12L10 14H4.17a2 2 0 0 1-1.92-2.56l2.33-8A2 2 0 0 1 6.5 2H20a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2.76a2 2 0 0 0-1.79 1.11L12 22a3.13 3.13 0 0 1-3-3.88M17 14V2')],
  'Chevron': [VeyloIconPath('m6 9l6 6l6-6')],
  'ChevronLeft': [VeyloIconPath('m15 18l-6-6l6-6')],
  'ChevronRight': [VeyloIconPath('m9 18l6-6l-6-6')],
  'Play': [VeyloIconPath('M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z')],
  'Check': [VeyloIconPath('M20 6L9 17l-5-5')],
  'PlayCircle': [VeyloIconPath('M9 9.003a1 1 0 0 1 1.517-.859l4.997 2.997a1 1 0 0 1 0 1.718l-4.997 2.997A1 1 0 0 1 9 14.996z'), VeyloIconCircle(12.0, 12.0, 10.0)],
  'Refresh': [VeyloIconPath('M3 12a9 9 0 0 1 9-9a9.75 9.75 0 0 1 6.74 2.74L21 8'), VeyloIconPath('M21 3v5h-5m5 4a9 9 0 0 1-9 9a9.75 9.75 0 0 1-6.74-2.74L3 16'), VeyloIconPath('M8 16H3v5')],
  'House': [VeyloIconPath('M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8'), VeyloIconPath('M3 10a2 2 0 0 1 .709-1.528l7-6a2 2 0 0 1 2.582 0l7 6A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z')],
  'Sparkles': [VeyloIconPath('M11.017 2.814a1 1 0 0 1 1.966 0l1.051 5.558a2 2 0 0 0 1.594 1.594l5.558 1.051a1 1 0 0 1 0 1.966l-5.558 1.051a2 2 0 0 0-1.594 1.594l-1.051 5.558a1 1 0 0 1-1.966 0l-1.051-5.558a2 2 0 0 0-1.594-1.594l-5.558-1.051a1 1 0 0 1 0-1.966l5.558-1.051a2 2 0 0 0 1.594-1.594zM20 2v4m2-2h-4'), VeyloIconCircle(4.0, 20.0, 2.0)],
  'Stack': [VeyloIconPath('m16 6l4 14M12 6v14M8 8v12M4 4v16')],
  'Sliders': [VeyloIconPath('M10 5H3m9 14H3M14 3v4m2 10v4m5-9h-9m9 7h-5m5-14h-7m-6 5v4m0-2H3')],
  'Grid': [VeyloIconRect(3.0, 3.0, 7.0, 7.0, 1.0, 1.0), VeyloIconRect(14.0, 3.0, 7.0, 7.0, 1.0, 1.0), VeyloIconRect(14.0, 14.0, 7.0, 7.0, 1.0, 1.0), VeyloIconRect(3.0, 14.0, 7.0, 7.0, 1.0, 1.0)],
  'Film': [VeyloIconRect(3.0, 3.0, 18.0, 18.0, 2.0, 2.0), VeyloIconPath('M7 3v18M3 7.5h4M3 12h18M3 16.5h4M17 3v18m0-13.5h4m-4 9h4')],
  'Tv': [VeyloIconPath('m17 2l-5 5l-5-5'), VeyloIconRect(2.0, 7.0, 20.0, 15.0, 2.0, 2.0)],
  'Blossom': [VeyloIconPath('M12 5a3 3 0 1 1 3 3m-3-3a3 3 0 1 0-3 3m3-3v1M9 8a3 3 0 1 0 3 3M9 8h1m5 0a3 3 0 1 1-3 3m3-3h-1m-2 3v-1'), VeyloIconCircle(12.0, 8.0, 2.0), VeyloIconPath('M12 10v12m0 0c4.2 0 7-1.667 7-5c-4.2 0-7 1.667-7 5m0 0c-4.2 0-7-1.667-7-5c4.2 0 7 1.667 7 5')],
  'Tag': [VeyloIconPath('M12.586 2.586A2 2 0 0 0 11.172 2H4a2 2 0 0 0-2 2v7.172a2 2 0 0 0 .586 1.414l8.704 8.704a2.426 2.426 0 0 0 3.42 0l6.58-6.58a2.426 2.426 0 0 0 0-3.42z'), VeyloIconCircle(7.5, 7.5, 0.5)],
  'Calendar': [VeyloIconPath('M8 2v3m8-3v3'), VeyloIconRect(3.0, 3.0, 18.0, 18.0, 2.0, 2.0), VeyloIconPath('M3 9h18')],
  'Info': [VeyloIconCircle(12.0, 12.0, 10.0), VeyloIconPath('M12 16v-4m0-4h.01')],
  'Star': [VeyloIconPath('M11.525 2.295a.53.53 0 0 1 .95 0l2.31 4.679a2.12 2.12 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904l-3.736 3.638a2.12 2.12 0 0 0-.611 1.878l.882 5.14a.53.53 0 0 1-.771.56l-4.618-2.428a2.12 2.12 0 0 0-1.973 0L6.396 21.01a.53.53 0 0 1-.77-.56l.881-5.139a2.12 2.12 0 0 0-.611-1.879L2.16 9.795a.53.53 0 0 1 .294-.906l5.165-.755a2.12 2.12 0 0 0 1.597-1.16z')],
  'Heart': [VeyloIconPath('M2 9.5a5.5 5.5 0 0 1 9.591-3.676a.56.56 0 0 0 .818 0A5.49 5.49 0 0 1 22 9.5c0 2.29-1.5 4-3 5.5l-5.492 5.313a2 2 0 0 1-3 .019L5 15c-1.5-1.5-3-3.2-3-5.5')],
  'ArrowRight': [VeyloIconPath('M5 12h14m-7-7l7 7l-7 7')],
  'Undo': [VeyloIconPath('M9 14L4 9l5-5'), VeyloIconPath('M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5a5.5 5.5 0 0 1-5.5 5.5H11')],
  'Backspace': [VeyloIconPath('M10 5a2 2 0 0 0-1.344.519l-6.328 5.74a1 1 0 0 0 0 1.481l6.328 5.741A2 2 0 0 0 10 19h10a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2zm2 4l6 6m0-6l-6 6')],
  'Trash': [VeyloIconPath('M10 11v6m4-6v6m5-11v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2')],
  'Alert': [VeyloIconPath('m21.73 18l-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3M12 9v4m0 4h.01')],
  'Cpu': [VeyloIconPath('M12 20v2m0-20v2m5 16v2m0-20v2M2 12h2m-2 5h2M2 7h2m16 5h2m-2 5h2M20 7h2M7 20v2M7 2v2'), VeyloIconRect(4.0, 4.0, 16.0, 16.0, 2.0, 2.0), VeyloIconRect(8.0, 8.0, 8.0, 8.0, 1.0, 1.0)],
  'Magnet': [VeyloIconPath('m12 15l4 4M2.352 10.648a1.205 1.205 0 0 0 0 1.704l2.296 2.296a1.205 1.205 0 0 0 1.704 0l6.029-6.029a1 1 0 1 1 3 3l-6.029 6.029a1.205 1.205 0 0 0 0 1.704l2.296 2.296a1.205 1.205 0 0 0 1.704 0l6.365-6.367A1 1 0 0 0 8.716 4.282zM5 8l4 4')],
  'Link': [VeyloIconPath('M9 17H7A5 5 0 0 1 7 7h2m6 0h2a5 5 0 1 1 0 10h-2m-7-5h8')],
  'ArrowLeft': [VeyloIconPath('m12 19l-7-7l7-7m7 7H5')],
  'Bookmark': [VeyloIconPath('M17 3a2 2 0 0 1 2 2v15a1 1 0 0 1-1.496.868l-4.512-2.578a2 2 0 0 0-1.984 0l-4.512 2.578A1 1 0 0 1 5 20V5a2 2 0 0 1 2-2z')],
  'Clock': [VeyloIconCircle(12.0, 12.0, 10.0), VeyloIconPath('M12 6v6l4 2')],
  'Clapper': [VeyloIconPath('m12.296 3.464l3.02 3.956M20.2 6L3 11l-.9-2.4c-.3-1.1.3-2.2 1.3-2.5l13.5-4c1.1-.3 2.2.3 2.5 1.3zM3 11h18v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2zm3.18-5.724l3.1 3.899')],
  'Users': [VeyloIconPath('M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2M16 3.128a4 4 0 0 1 0 7.744M22 21v-2a4 4 0 0 0-3-3.87'), VeyloIconCircle(9.0, 7.0, 4.0)],
  'Pen': [VeyloIconPath('M13 21h8m.174-14.188a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z')],
  'Globe': [VeyloIconCircle(12.0, 12.0, 10.0), VeyloIconPath('M12 2a14.5 14.5 0 0 0 0 20a14.5 14.5 0 0 0 0-20M2 12h20')],
  'Speech': [VeyloIconPath('M2.992 16.342a2 2 0 0 1 .094 1.167l-1.065 3.29a1 1 0 0 0 1.236 1.168l3.413-.998a2 2 0 0 1 1.099.092a10 10 0 1 0-4.777-4.719')],
  'Trophy': [VeyloIconPath('M10 14.66V17a1 1 0 0 1-1 1a2 2 0 0 0-2 2v2m7-7.34V17a1 1 0 0 0 1 1a2 2 0 0 1 2 2v2m.916-12H19.5A2.5 2.5 0 0 0 22 7.5V5a1 1 0 0 0-1-1h-3M4 22h16'), VeyloIconPath('M6 9a6 6 0 0 0 12 0V3a1 1 0 0 0-1-1H7a1 1 0 0 0-1 1z'), VeyloIconPath('M6.084 10H4.5A2.5 2.5 0 0 1 2 7.5V5a1 1 0 0 1 1-1h3')],
  'List': [VeyloIconPath('M3 5h.01M3 12h.01M3 19h.01M8 5h13M8 12h13M8 19h13')],
  'EyeOff': [VeyloIconPath('M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575a1 1 0 0 1 0 .696a10.8 10.8 0 0 1-1.444 2.49m-6.41-.679a3 3 0 0 1-4.242-4.242'), VeyloIconPath('M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151a1 1 0 0 1 0-.696a10.75 10.75 0 0 1 4.446-5.143M2 2l20 20')],
  'Camera': [VeyloIconPath('m16 13l5.223 3.482a.5.5 0 0 0 .777-.416V7.87a.5.5 0 0 0-.752-.432L16 10.5'), VeyloIconRect(2.0, 6.0, 14.0, 12.0, 2.0, 2.0)],
  'Layers': [VeyloIconPath('M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z'), VeyloIconPath('M2 12a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 12'), VeyloIconPath('M2 17a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 17')],
  'Next': [VeyloIconPath('M21 4v16M6.029 4.285A2 2 0 0 0 3 6v12a2 2 0 0 0 3.029 1.715l9.997-5.998a2 2 0 0 0 .003-3.432z')],
  'Pause': [VeyloIconRect(14.0, 3.0, 5.0, 18.0, 1.0, 1.0), VeyloIconRect(5.0, 3.0, 5.0, 18.0, 1.0, 1.0)],
  'Rewind10': [VeyloIconPath('M3 12a9 9 0 1 0 9-9a9.75 9.75 0 0 0-6.74 2.74L3 8'), VeyloIconPath('M3 3v5h5'), VeyloIconPath('M9.5 10.5v4.5M12.5 10.5h1.5a1 1 0 0 1 1 1v2.5a1 1 0 0 1-1 1h-1.5a1 1 0 0 1-1-1v-2.5a1 1 0 0 1 1-1Z')],
  'Forward10': [VeyloIconPath('M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8'), VeyloIconPath('M21 3v5h-5'), VeyloIconPath('M9.5 10.5v4.5M12.5 10.5h1.5a1 1 0 0 1 1 1v2.5a1 1 0 0 1-1 1h-1.5a1 1 0 0 1-1-1v-2.5a1 1 0 0 1 1-1Z')],
  'Audio': [VeyloIconPath('M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298zM16 9a5 5 0 0 1 0 6m3.364 3.364a9 9 0 0 0 0-12.728')],
  'Muted': [VeyloIconPath('M11 4.702a.7.7 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.7.7 0 0 0 11 19.298zm5.5 9.798l5-5m-5 0l5 5')],
  'Subtitles': [VeyloIconRect(3.0, 5.0, 18.0, 14.0, 2.0, 2.0), VeyloIconPath('M7 15h4m4 0h2M7 11h2m4 0h4')],
  'Gauge': [VeyloIconPath('m12 14l4-4M3.34 19a10 10 0 1 1 17.32 0')],
  'Resize': [VeyloIconPath('M15 3h6v6m0-6l-7 7M3 21l7-7m-1 7H3v-6')],
  'Expand': [VeyloIconPath('M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3M3 16v3a2 2 0 0 0 2 2h3m8 0h3a2 2 0 0 0 2-2v-3')],
  'Moon': [VeyloIconPath('M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401')],
  'Keyboard': [VeyloIconPath('M10 8h.01M12 12h.01M14 8h.01M16 12h.01M18 8h.01M6 8h.01M7 16h10m-9-4h.01'), VeyloIconRect(2.0, 4.0, 20.0, 16.0, 2.0, 2.0)],
  'Lock': [VeyloIconRect(3.0, 11.0, 18.0, 11.0, 2.0, 2.0), VeyloIconPath('M7 11V7a5 5 0 0 1 10 0v4')],
  'Unlock': [VeyloIconRect(3.0, 11.0, 18.0, 11.0, 2.0, 2.0), VeyloIconPath('M7 11V7a5 5 0 0 1 9.9-1')],
  'Pip': [VeyloIconPath('M21 9V6a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v10c0 1.1.9 2 2 2h4'), VeyloIconRect(12.0, 13.0, 10.0, 7.0, 2.0, 2.0)],
  'Copy': [VeyloIconRect(8.0, 8.0, 14.0, 14.0, 2.0, 2.0), VeyloIconPath('M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2')],
  'Upload': [VeyloIconPath('M12 3v12m5-7l-5-5l-5 5m14 7v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4')],
  'Plus': [VeyloIconPath('M5 12h14m-7-7v14')],
  'Headphones': [VeyloIconPath('M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3')],
  'RotateScreen': [VeyloIconPath('M12 5H6a2 2 0 0 0-2 2v3'), VeyloIconPath('m9 8l3-3l-3-3M4 14v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-2')],
};
