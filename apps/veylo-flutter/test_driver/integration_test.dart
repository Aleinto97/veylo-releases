// Writes the runtime test's report and screenshots to build/runtime/ for CI (artifact upload +
// `sentry snapshots upload`) — also when steps failed, which is when they matter most (the stock
// integrationDriver only hands data over on success).
import 'dart:convert';
import 'dart:io';

import 'package:flutter_driver/flutter_driver.dart';
import 'package:integration_test/common.dart';

Future<void> main() async {
  final driver = await FlutterDriver.connect();
  final raw = await driver.requestData(null, timeout: const Duration(minutes: 40));
  await driver.close();
  final response = Response.fromJson(raw);
  final data = response.data;
  if (data != null) {
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
  } else {
    stdout.writeln('runtime report: no data (the test did not reach its end)');
  }
  if (!response.allTestsPassed) {
    for (final f in response.failureDetails ?? const <Failure>[]) {
      stderr.writeln('FAILED: ${f.methodName}: ${f.details}');
    }
    exit(1);
  }
  exit(0);
}
