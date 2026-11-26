#!/usr/bin/env bash
# Launch check for the desktop app on macOS and Linux (CI): start Veylo with a fresh, temporary
# library, let it run, fail if it exits or panics, and save a screenshot of the screen.
#
#   tools/ci/launch-check.sh <veylo-dx executable> <screenshot.png> [seconds] [--demo | --bars]
#
# --demo also plays the test stream at launch (VEYLO_DX_PLAY_DEMO). --bars is the picture test: it
# plays six colour bars, and the screenshot (what the OS really shows) must show them, in order
# (`veylo-dx --check-picture`, crates/picture-check). VEYLO_EXPECT_LOG=<text> also requires that
# line in the app's output. Linux needs an X display:
# run it under `xvfb-run -a -s "-screen 0 1440x900x24"`.
set -euo pipefail
exe=$1 shot=$2 seconds=${3:-25}
data=$(mktemp -d)
log="$data/veylo.log"
pid=
trap '[ -n "$pid" ] && kill "$pid" 2>/dev/null; rm -rf "$data"' EXIT
export VEYLO_DX_DATA="$data"
[ "${4:-}" = --demo ] && export VEYLO_DX_PLAY_DEMO=1
[ "${4:-}" = --bars ] && export VEYLO_DX_PLAY_DEMO=bars
mkdir -p "$(dirname "$shot")"

"$exe" >"$log" 2>&1 &
pid=$!
elapsed=0
while [ "$elapsed" -lt "$seconds" ]; do
    sleep 1
    elapsed=$((elapsed + 1))
    if ! kill -0 "$pid" 2>/dev/null; then
        wait "$pid" && code=0 || code=$?
        echo "Veylo exited (code $code) after ${elapsed}s. Its output:" >&2
        cat "$log" >&2
        [ -f "$data/last-panic.txt" ] && cat "$data/last-panic.txt" >&2
        exit 1
    fi
    if [ -f "$data/last-panic.txt" ]; then
        echo "Veylo panicked after ${elapsed}s:" >&2; cat "$data/last-panic.txt" >&2; exit 1
    fi
done
if [ -n "${VEYLO_EXPECT_LOG:-}" ] && ! grep -q -F "$VEYLO_EXPECT_LOG" "$log"; then
    echo "Veylo never logged \"$VEYLO_EXPECT_LOG\". Its output:" >&2; cat "$log" >&2; exit 1
fi

case "$(uname -s)" in
    Darwin) screencapture -x "$shot" ;;
    *) import -window root "$shot"; xwininfo -root -tree 2>/dev/null | grep -i -E 'veylo|tao|child' | head -20 || true ;;
esac
echo "Veylo ran for $seconds s; screenshot: $shot"
[ -s "$log" ] && { echo "Its output:"; cat "$log"; }
# A screen of one or two colours means nothing was drawn (where ImageMagick can count them).
if command -v identify >/dev/null; then
    colours=$(identify -format %k "$shot")
    if [ "$colours" -le 2 ]; then echo "The screenshot is blank ($colours colours): the window drew nothing." >&2; exit 1; fi
    echo "Screenshot has $colours colours."
fi
if [ "${4:-}" = --bars ]; then
    grep -F "picture path" "$log" || echo "Veylo logged no picture path." >&2
    "$exe" --check-picture "$shot" || exit 1
fi
exit 0
