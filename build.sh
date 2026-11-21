#!/usr/bin/env bash
# Build Kotatsu Desktop from source.
#   ./build.sh              build (tuned for this CPU) and install into ~/.local
#   ./build.sh --update     also pull the latest parsers (new/fixed sources) first
#   ./build.sh --portable   build for any x86-64 CPU (what releases use)
#   ./build.sh --package    also write dist/kotatsu-desktop-linux-x86_64.tar.gz
#   ./build.sh --no-install don't install into ~/.local
#
# Don't want to build? Use a prebuilt release instead:
#   curl -fsSL https://raw.githubusercontent.com/justinsus1/kotatsu-desktop/main/install.sh | bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
TOOLS="$ROOT/.tools"

UPDATE=0 PORTABLE=0 PACKAGE=0 INSTALL=1
for a in "$@"; do
  case "$a" in
    --update) UPDATE=1 ;;
    --portable) PORTABLE=1 ;;
    --package) PACKAGE=1 ;;
    --no-install) INSTALL=0 ;;
    *) echo "unknown option $a" >&2; exit 2 ;;
  esac
done

# Sources library (git submodule)
if [[ ! -f "$ROOT/parsers/build.gradle.kts" ]]; then
  git -C "$ROOT" submodule update --init --depth 1 parsers
fi
if [[ $UPDATE == 1 ]]; then
  git -C "$ROOT/parsers" pull --ff-only origin HEAD
fi

# Toolchains: use JAVA17_HOME / GRAALVM_HOME if given (CI), otherwise download once into .tools/
mkdir -p "$TOOLS"
if [[ -n "${JAVA17_HOME:-}" ]]; then
  export JAVA_HOME="$JAVA17_HOME"
else
  export JAVA_HOME="$TOOLS/jdk17"
  if [[ ! -x "$JAVA_HOME/bin/java" ]]; then
    echo "==> downloading JDK 17"
    curl -fsSL "https://api.adoptium.net/v3/binary/latest/17/ga/linux/x64/jdk/hotspot/normal/eclipse" -o "$TOOLS/jdk17.tgz"
    mkdir -p "$TOOLS/jdk17" && tar xzf "$TOOLS/jdk17.tgz" -C "$TOOLS/jdk17" --strip-components=1 && rm "$TOOLS/jdk17.tgz"
  fi
fi
GRAAL="${GRAALVM_HOME:-$TOOLS/graalvm}"
if [[ ! -x "$GRAAL/bin/native-image" ]]; then
  echo "==> downloading GraalVM"
  # Read the whole response first: piping curl into `head` makes curl fail with
  # "failure writing output" once head exits, which kills the script under pipefail
  RELEASES=$(curl -fsSL https://api.github.com/repos/graalvm/graalvm-ce-builds/releases)
  URL=$(grep -o 'https://[^"]*graalvm-community-jdk-25[^"]*linux-x64_bin\.tar\.gz' <<<"$RELEASES" || true)
  URL=${URL%%$'\n'*}
  if [[ -z "$URL" ]]; then
    echo "Could not find a GraalVM download (GitHub API rate limit?). Try again later." >&2
    exit 1
  fi
  curl -fL --progress-bar "$URL" -o "$TOOLS/graal.tgz"
  mkdir -p "$GRAAL" && tar xzf "$TOOLS/graal.tgz" -C "$GRAAL" --strip-components=1 && rm "$TOOLS/graal.tgz"
fi

if [[ $PORTABLE == 1 ]]; then
  MARCH="-march=compatibility"
  export RUSTFLAGS="${RUSTFLAGS:-}"
else
  MARCH="-march=native"
  export RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=native"
fi

echo "==> parsers"
(cd "$ROOT/parsers" && ./gradlew -q jar)
echo "==> bridge (jar)"
(cd "$ROOT/bridge" && ./gradlew -q fatJar)
echo "==> bridge (native-image)"
mkdir -p "$ROOT/bridge/build/native"
(cd "$ROOT/bridge/build/native" && "$GRAAL/bin/native-image" -jar ../libs/kotatsu-bridge-all.jar \
  -o kotatsu-bridge --no-fallback $MARCH --enable-url-protocols=http,https \
  -Djava.awt.headless=true -J-Xmx6g >/dev/null)
echo "==> app"
(cd "$ROOT/app" && cargo build --release -q)

# Same layout as ~/.local: bin/, lib/kotatsu-desktop/, share/applications/
stage() {
  local dest="$1" exec_path="$2"
  mkdir -p "$dest/bin" "$dest/lib/kotatsu-desktop" "$dest/share/applications"
  install -m755 "$ROOT/app/target/release/kotatsu-desktop" "$dest/bin/kotatsu-desktop"
  install -m755 "$ROOT/bridge/build/native/kotatsu-bridge" "$dest/lib/kotatsu-desktop/kotatsu-bridge"
  cp "$ROOT"/bridge/build/native/*.so "$dest/lib/kotatsu-desktop/" 2>/dev/null || true
  cat > "$dest/share/applications/kotatsu-desktop.desktop" <<EOF
[Desktop Entry]
Name=Kotatsu
Comment=Manga and book reader
Exec=$exec_path
Icon=accessories-ebook-reader
Terminal=false
Type=Application
Categories=Graphics;Viewer;
StartupWMClass=kotatsu-desktop
EOF
}

if [[ $PACKAGE == 1 ]]; then
  echo "==> package"
  STAGE="$ROOT/dist/kotatsu-desktop"
  rm -rf "$STAGE" && mkdir -p "$STAGE"
  stage "$STAGE" "kotatsu-desktop"
  cp "$ROOT/LICENSE" "$ROOT/README.md" "$STAGE/"
  tar -C "$ROOT/dist" -czf "$ROOT/dist/kotatsu-desktop-linux-x86_64.tar.gz" kotatsu-desktop
  echo "Package: $ROOT/dist/kotatsu-desktop-linux-x86_64.tar.gz"
fi

if [[ $INSTALL == 1 ]]; then
  echo "==> install"
  stage "$HOME/.local" "$HOME/.local/bin/kotatsu-desktop"
  echo "Installed: $HOME/.local/bin/kotatsu-desktop"
fi
