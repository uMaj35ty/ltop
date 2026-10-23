#!/usr/bin/env bash
# Assemble the GitHub Pages site: mdBook (en + zh) under docs/, landing page
# at the root, screenshots under assets/. Used verbatim by CI
# (.github/workflows/site.yml) and locally for previewing:
#   ./scripts/build-site.sh dist/site && python3 -m http.server -d dist/site 8000
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="${1:-dist/site}"

command -v mdbook >/dev/null || { echo "mdbook not found: cargo install mdbook" >&2; exit 1; }

rm -rf "$OUT"
mkdir -p "$OUT"

mdbook build book -d "$OUT/docs"
mdbook build book-zh -d "$OUT/docs/zh"

# landing pages (en + zh) + shared screenshots (book chapters reference ../assets/)
cp website/*.html website/style.css "$OUT/"
cp -r assets "$OUT/assets"
touch "$OUT/.nojekyll"

echo "site assembled in $OUT ($(find "$OUT" -type f | wc -l) files)"
