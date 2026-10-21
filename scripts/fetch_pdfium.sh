#!/usr/bin/env bash
# Downloads the pinned PDFium build (third_party/pdfium/PDFIUM_VERSION.txt), verifies its SHA-256 and
# unpacks the shared library + licences into third_party/pdfium/<platform>/ (git-ignored).
#
#   scripts/fetch_pdfium.sh               # linux-x64
#   scripts/fetch_pdfium.sh win-x64       # pdfium.dll for Windows packaging
#
# Then: export RV_PDFIUM_PATH="$PWD/third_party/pdfium/linux-x64/libpdfium.so"
# A mismatching digest aborts without leaving a library behind.
set -euo pipefail

platform="${1:-linux-x64}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pin="$root/third_party/pdfium/PDFIUM_VERSION.txt"

value() { grep -E "^$1=" "$pin" | head -n 1 | cut -d= -f2-; }
tag="$(value tag)"
expected="$(value "sha256.$platform")"
if [ -z "$tag" ] || [ -z "$expected" ]; then
  echo "error: no tag or sha256.$platform in $pin" >&2
  exit 2
fi

case "$platform" in
  win-*) member="bin/pdfium.dll"; lib="pdfium.dll" ;;
  mac-*) member="lib/libpdfium.dylib"; lib="libpdfium.dylib" ;;
  *) member="lib/libpdfium.so"; lib="libpdfium.so" ;;
esac

dest="$root/third_party/pdfium/$platform"
if [ -f "$dest/$lib" ] && [ -f "$dest/ARCHIVE_SHA256" ] && [ "$(cat "$dest/ARCHIVE_SHA256")" = "$expected" ]; then
  echo "$dest/$lib (already present, $tag)"
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
url="https://github.com/bblanchon/pdfium-binaries/releases/download/${tag//\//%2F}/pdfium-$platform.tgz"
echo "downloading $url" >&2
curl -sSfL --retry 3 -o "$work/pdfium.tgz" "$url"

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work/pdfium.tgz" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$work/pdfium.tgz" | cut -d' ' -f1)"
fi
if [ "$actual" != "$expected" ]; then
  echo "error: SHA-256 mismatch for pdfium-$platform.tgz: expected $expected, got $actual" >&2
  exit 1
fi

mkdir -p "$work/x"
tar xzf "$work/pdfium.tgz" -C "$work/x" "$member" LICENSE VERSION licenses
rm -rf "$dest"
mkdir -p "$dest"
cp "$work/x/$member" "$dest/$lib"
cp "$work/x/LICENSE" "$dest/LICENSE-pdfium-binaries.txt"
cp "$work/x/VERSION" "$dest/VERSION"
cp -R "$work/x/licenses" "$dest/licenses"
printf '%s' "$expected" > "$dest/ARCHIVE_SHA256"
echo "$dest/$lib"
