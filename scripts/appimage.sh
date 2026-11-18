#!/usr/bin/env bash
# Gera gitmeta-<versão>-x86_64.AppImage a partir do binário release.
# Uso: scripts/appimage.sh <versão> [caminho-do-binario]
set -euo pipefail
VERSAO="${1:?uso: appimage.sh <versão> [binario]}"
BIN="${2:-target/release/gitmeta}"
ARQ="$(uname -m)"
cd "$(dirname "$0")/.."

rm -rf AppDir && mkdir -p AppDir/usr/bin
install -m755 "$BIN" AppDir/usr/bin/gitmeta
install -m644 packaging/gitmeta.desktop AppDir/gitmeta.desktop
install -m644 packaging/gitmeta.png AppDir/gitmeta.png
cat > AppDir/AppRun <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/gitmeta" "$@"
RUN
chmod +x AppDir/AppRun

TOOL="appimagetool-${ARQ}.AppImage"
if [ ! -x "$TOOL" ]; then
  curl -fsSL -o "$TOOL" "https://github.com/AppImage/appimagetool/releases/download/continuous/$TOOL"
  chmod +x "$TOOL"
fi
# --appimage-extract-and-run: não exige FUSE (CI e contêineres).
ARCH="$ARQ" "./$TOOL" --appimage-extract-and-run AppDir "gitmeta-${VERSAO}-${ARQ}.AppImage"
ls -la "gitmeta-${VERSAO}-${ARQ}.AppImage"
