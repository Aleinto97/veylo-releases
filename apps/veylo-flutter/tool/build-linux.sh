#!/usr/bin/env bash
# Release build of the Flutter Linux app with Sentry crash reporting:
#   1. bakes the DSN in (VEYLO_SENTRY_DSN, or ~/.config/veylo/sentry-dsn; never committed),
#   2. uploads the bridge's line tables to Sentry so panics show file:line,
#   3. strips debug info from the shipped .so (25 MB instead of ~57 MB).
# Upload needs a logged-in `sentry` CLI (sentry auth status); it is skipped otherwise.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ -z "${VEYLO_SENTRY_DSN:-}" && -r "$HOME/.config/veylo/sentry-dsn" ]]; then
  VEYLO_SENTRY_DSN="$(tr -d '[:space:]' < "$HOME/.config/veylo/sentry-dsn")"
fi
export VEYLO_SENTRY_DSN="${VEYLO_SENTRY_DSN:-}"
[[ -n "$VEYLO_SENTRY_DSN" ]] || echo "note: no Sentry DSN; crash reporting will be inert in this build" >&2
(cd rust && cargo build --release)
flutter build linux --release
so="build/linux/$(uname -m | sed 's/aarch64/arm64/;s/x86_64/x64/')/release/bundle/lib/libveylo_bridge.so"
if [[ -n "$VEYLO_SENTRY_DSN" ]] && command -v sentry >/dev/null && sentry auth status >/dev/null 2>&1; then
  SENTRY_ORG="${SENTRY_ORG:-veylo-sza}" SENTRY_PROJECT="${SENTRY_PROJECT:-veylo-flutter}" \
    sentry debug-files upload --wait rust/target/release/libveylo_bridge.so
else
  echo "note: debug files not uploaded (no DSN or sentry CLI not logged in)" >&2
fi
objcopy --strip-debug "$so"
echo "built: $so"
