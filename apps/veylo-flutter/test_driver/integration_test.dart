// Writes the runtime test's report and screenshots to build/runtime/<target>/ for CI
// (artifact upload + `sentry snapshots upload`).
import 'dart:convert';
import 'dart:io';

import 'package:integration_test/integration_test_driver.dart';

Future<void> main() => integrationDriver(responseDataCallback: (data) async {
      if (data == null) return;
      final target = data['target'] as String? ?? 'local';
      final root = Directory('build/runtime/$target')..createSync(recursive: true);
      File('${root.path}/report.json').writeAsStringSync(
        const JsonEncoder.withIndent('  ').convert({'target': target, 'results': data['results']}),
      );
      final shots = (data['screenshots'] as Map<String, dynamic>? ?? {});
      for (final e in shots.entries) {
        final f = File('build/runtime/snapshots/${e.key}.png')..parent.createSync(recursive: true);
        f.writeAsBytesSync(base64Decode(e.value as String));
      }
      stdout.writeln('runtime report: ${root.path}/report.json, ${shots.length} screenshots');
    });
