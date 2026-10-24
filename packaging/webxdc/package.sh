#!/usr/bin/env bash
# Build WordCraft as a webxdc app (Delta Chat and other messengers):  $DIST/wordcraft-<version>.xdc
#
# Usage: packaging/webxdc/package.sh [--skip-build]
#
# Needs what packaging/web/package.sh needs. The page (apps/wordcraft-web/webxdc.html) loads the
# messenger's webxdc.js, which the package never ships.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

SITE="$CARGO_TARGET_DIR/webxdc-site"
if [ "${1:-}" != "--skip-build" ]; then
  command -v trunk >/dev/null || { echo "error: trunk not found (cargo install trunk --locked)" >&2; exit 1; }
  # Every member lays the text out with the same fonts: only those bundled in the repository,
  # never a craft-fonts checkout a release build might point at.
  # Trunk writes the page as index.html, where the messenger looks for it.
  (cd "$ROOT/apps/wordcraft-web" && env -u CRAFT_FONTS_DIR trunk build webxdc.html --release --dist "$SITE" --public-url ./)
fi
[ -f "$SITE/index.html" ] || { echo "error: $SITE/index.html missing; run without --skip-build" >&2; exit 1; }
# Ship the wasm gzipped and stored (zip -n .gz below): Delta Chat inflates zip entries slowly, and
# the browser gunzips it natively while compiling.
# ponytail: needs DecompressionStream (Chrome 80, Safari 16.4, Firefox 113); no fallback.
for wasm in "$SITE"/*_bg.wasm; do
  [ -f "$wasm" ] || continue
  name=$(basename "$wasm")
  echo "wasm: $(du -h "$wasm" | cut -f1) raw"
  gzip -9 -n -f "$wasm"
  echo "wasm: $(du -h "$wasm.gz" | cut -f1) gzipped"
  NAME="$name" perl -pi -e '
    s{<link rel="preload" href="\./\Q$ENV{NAME}\E"[^>]*>}{};
    s{\x27\./\Q$ENV{NAME}\E\x27}{fetch("./$ENV{NAME}.gz").then(r => new Response(r.body.pipeThrough(new DecompressionStream("gzip")), { headers: { "content-type": "application/wasm" } }))};
  ' "$SITE/index.html"
done
# Also on a rerun with --skip-build, where there is no .wasm left to rewrite.
grep -q '_bg\.wasm\.gz' "$SITE/index.html" || { echo "error: index.html doesn't load the gzipped wasm (trunk's loader changed?)" >&2; exit 1; }

cp -f "$ROOT/assets/app-icon/wordcraft-macos-512.png" "$SITE/icon.png"
cat > "$SITE/manifest.toml" <<TOML
name = "WordCraft"
source_code_url = "https://github.com/experintellia/wordcraft"
TOML
copy_docs "$SITE"
# The bundled fonts are compiled into the wasm: their licences travel with it.
cp -f "$ROOT/NOTICE" "$ROOT"/assets/fonts/OFL-*.txt "$SITE/"
OUT="$DIST/wordcraft-$VERSION.xdc"
rm -f "$OUT"
# No -9: in zip 3.0 it overrides -n, which would deflate the .gz again.
(cd "$SITE" && zip -qr -n .gz "$OUT" .)
echo "wrote $OUT"
ls -lh "$OUT"
