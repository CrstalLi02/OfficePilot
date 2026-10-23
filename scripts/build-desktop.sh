#!/usr/bin/env bash
# 打包 OfficePilot 桌面版（macOS，arm64），把编辑器套件放进 OfficePilot.app/Contents/Resources/suite/。
#
#   scripts/build-desktop.sh              # 先构建套件，再构建桌面版
#   SKIP_SUITE_BUILD=1 scripts/build-desktop.sh   # 沿用已经构建好的套件
#
# 产物：desktop/src-tauri/target/release/bundle/macos/OfficePilot.app 与同目录的 dmg/OfficePilot_<版本>_aarch64.dmg
# 两个 .app 都未签名；签名与公证在发布流程里统一处理。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SUITE_APP="$ROOT/suite/apps/shell/release/mac-arm64/OfficePilot Suite.app"
TAURI="$ROOT/desktop/src-tauri"

[ "$(uname)" = "Darwin" ] || { echo "目前只支持在 macOS 上打包" >&2; exit 1; }

if [ "${SKIP_SUITE_BUILD:-}" != "1" ] || [ ! -d "$SUITE_APP" ]; then
  "$ROOT/scripts/build-suite.sh"
fi

# 服务端内嵌前端（rust-embed）
(cd "$ROOT/frontend" && npm run build)

# 先只打 .app：套件要在 dmg 之前放进去
(cd "$TAURI" && cargo tauri build --bundles app)

BUNDLE="$TAURI/target/release/bundle"
APP="$BUNDLE/macos/OfficePilot.app"
[ -d "$APP" ] || { echo "没有找到 $APP" >&2; exit 1; }

DEST="$APP/Contents/Resources/suite"
rm -rf "$DEST"
mkdir -p "$DEST"
# ditto 保留 Electron 框架里的符号链接和扩展属性
ditto "$SUITE_APP" "$DEST/OfficePilot Suite.app"

VERSION="$(sed -n 's/^ *"version": *"\(.*\)",/\1/p' "$TAURI/tauri.conf.json" | head -1)"
mkdir -p "$BUNDLE/dmg"
DMG="$BUNDLE/dmg/OfficePilot_${VERSION}_aarch64.dmg"
rm -f "$DMG"
STAGE="$(mktemp -d)"
ditto "$APP" "$STAGE/OfficePilot.app"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname OfficePilot -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

echo "OfficePilot.app: $(du -sh "$APP" | cut -f1)（其中编辑器套件 $(du -sh "$DEST" | cut -f1)）"
echo "dmg: $DMG（$(du -sh "$DMG" | cut -f1)）"
