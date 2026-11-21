#!/usr/bin/env bash
# Build + install Kotatsu Desktop into ~/.local
#   ./build.sh            build everything and install
#   ./build.sh --update   also pull the latest parsers (new/fixed sources) first
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
TOOLS="$ROOT/.tools"
export JAVA_HOME="$TOOLS/jdk17"

# Sources library (git submodule)
if [[ ! -f "$ROOT/parsers/build.gradle.kts" ]]; then
  git -C "$ROOT" submodule update --init --depth 1 parsers
fi
if [[ "${1:-}" == "--update" ]]; then
  git -C "$ROOT/parsers" pull --ff-only origin HEAD
fi

# Toolchains (downloaded once into .tools/, nothing installed system-wide)
mkdir -p "$TOOLS"
if [[ ! -x "$TOOLS/jdk17/bin/java" ]]; then
  echo "==> downloading JDK 17"
  curl -fsSL "https://api.adoptium.net/v3/binary/latest/17/ga/linux/x64/jdk/hotspot/normal/eclipse" -o "$TOOLS/jdk17.tgz"
  mkdir -p "$TOOLS/jdk17" && tar xzf "$TOOLS/jdk17.tgz" -C "$TOOLS/jdk17" --strip-components=1 && rm "$TOOLS/jdk17.tgz"
fi
if [[ ! -x "$TOOLS/graalvm/bin/native-image" ]]; then
  echo "==> downloading GraalVM"
  URL=$(curl -fsSL https://api.github.com/repos/graalvm/graalvm-ce-builds/releases \
    | grep -o 'https://[^"]*graalvm-community-jdk-25[^"]*linux-x64_bin.tar.gz' | head -1)
  curl -fsSL "$URL" -o "$TOOLS/graal.tgz"
  mkdir -p "$TOOLS/graalvm" && tar xzf "$TOOLS/graal.tgz" -C "$TOOLS/graalvm" --strip-components=1 && rm "$TOOLS/graal.tgz"
fi

echo "==> parsers"
(cd "$ROOT/parsers" && ./gradlew -q jar)
echo "==> bridge (jar)"
(cd "$ROOT/bridge" && ./gradlew -q fatJar)
echo "==> bridge (native-image)"
mkdir -p "$ROOT/bridge/build/native"
(cd "$ROOT/bridge/build/native" && "$TOOLS/graalvm/bin/native-image" -jar ../libs/kotatsu-bridge-all.jar \
  -o kotatsu-bridge --no-fallback -march=native --enable-url-protocols=http,https \
  -Djava.awt.headless=true -J-Xmx10g >/dev/null)
echo "==> app"
(cd "$ROOT/app" && cargo build --release -q)

echo "==> install"
LIB="$HOME/.local/lib/kotatsu-desktop"
mkdir -p "$LIB" "$HOME/.local/bin" "$HOME/.local/share/applications"
install -m755 "$ROOT/app/target/release/kotatsu-desktop" "$HOME/.local/bin/kotatsu-desktop"
install -m755 "$ROOT/bridge/build/native/kotatsu-bridge" "$LIB/kotatsu-bridge"
cp "$ROOT"/bridge/build/native/*.so "$LIB/" 2>/dev/null || true
cat > "$HOME/.local/share/applications/kotatsu-desktop.desktop" <<EOF
[Desktop Entry]
Name=Kotatsu
Comment=Manga reader
Exec=$HOME/.local/bin/kotatsu-desktop
Icon=accessories-ebook-reader
Terminal=false
Type=Application
Categories=Graphics;Viewer;
StartupWMClass=kotatsu-desktop
EOF
echo "Installed: $HOME/.local/bin/kotatsu-desktop"
