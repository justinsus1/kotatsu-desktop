#!/usr/bin/env bash
# Install the latest prebuilt Kotatsu Desktop (no compiling) into ~/.local
#   curl -fsSL https://raw.githubusercontent.com/justinsus1/kotatsu-desktop/main/install.sh | bash
#   ... | bash -s -- --uninstall
set -euo pipefail
REPO="justinsus1/kotatsu-desktop"
ASSET="kotatsu-desktop-linux-x86_64.tar.gz"
PREFIX="${PREFIX:-$HOME/.local}"

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -f "$PREFIX/bin/kotatsu-desktop" "$PREFIX/share/applications/kotatsu-desktop.desktop"
  rm -rf "$PREFIX/lib/kotatsu-desktop"
  echo "Uninstalled (your library and settings in ~/.local/share/kotatsu-desktop were kept)."
  exit 0
fi

if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  echo "Prebuilt releases are Linux x86_64 only; build from source with ./build.sh" >&2
  exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
echo "Downloading the latest release…"
curl -fL --progress-bar "https://github.com/$REPO/releases/latest/download/$ASSET" -o "$TMP/$ASSET"
tar -xzf "$TMP/$ASSET" -C "$TMP"
SRC="$TMP/kotatsu-desktop"

mkdir -p "$PREFIX/bin" "$PREFIX/lib/kotatsu-desktop" "$PREFIX/share/applications"
install -m755 "$SRC/bin/kotatsu-desktop" "$PREFIX/bin/kotatsu-desktop"
cp -r "$SRC/lib/kotatsu-desktop/." "$PREFIX/lib/kotatsu-desktop/"
chmod 755 "$PREFIX/lib/kotatsu-desktop/kotatsu-bridge"
sed "s|^Exec=.*|Exec=$PREFIX/bin/kotatsu-desktop|" "$SRC/share/applications/kotatsu-desktop.desktop" \
  > "$PREFIX/share/applications/kotatsu-desktop.desktop"

echo "Installed $PREFIX/bin/kotatsu-desktop"
case ":$PATH:" in
  *":$PREFIX/bin:"*) ;;
  *) echo "Note: $PREFIX/bin is not on your PATH; launch it from your app menu or add it to PATH." ;;
esac
