#!/usr/bin/env bash
# Render site/og.jpg (the 1200x630 picture link previews show) from scripts/og-image.html with
# headless Chrome, plus the PNG icons from site/favicon.svg. Needs google-chrome and ImageMagick.
# Usage: scripts/og-image.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
shot() { # <html file> <width> <height> <png out>
  google-chrome --headless=new --no-sandbox --disable-gpu --hide-scrollbars --allow-file-access-from-files \
    --default-background-color=00000000 --force-device-scale-factor=1 --window-size="$2,$3" \
    --virtual-time-budget=4000 --screenshot="$4" "file://$1" >/dev/null 2>&1
  test -s "$4"
}
shot "$ROOT/scripts/og-image.html" 1200 630 "$WORK/og.png"
convert "$WORK/og.png" -strip -sampling-factor 4:2:0 -quality 86 -interlace JPEG "$ROOT/site/og.jpg"
for n in 180 32; do
  printf '<!doctype html><style>html,body{margin:0;width:%spx;height:%spx;overflow:hidden;background:transparent}img{width:%spx;height:%spx;display:block}</style><img src="file://%s/site/favicon.svg">' \
    "$n" "$n" "$n" "$n" "$ROOT" > "$WORK/icon.html"
  shot "$WORK/icon.html" "$n" "$n" "$WORK/icon-$n.png"
done
cp "$WORK/icon-180.png" "$ROOT/site/apple-touch-icon.png"
cp "$WORK/icon-32.png" "$ROOT/site/favicon-32.png"
identify "$ROOT/site/og.jpg" "$ROOT/site/apple-touch-icon.png" "$ROOT/site/favicon-32.png"
