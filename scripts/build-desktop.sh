#!/usr/bin/env bash
# 打包 OfficePilot 桌面版（macOS，arm64）：编辑器套件外壳 + OfficePilot 服务，一个应用。
#
#   scripts/build-desktop.sh
#
# 产物：suite/apps/shell/release/OfficePilot-<版本>-arm64.dmg 与 release/mac-arm64/OfficePilot.app。
# 服务端（walioffice）和命令行（officepilot）放在 OfficePilot.app/Contents/Resources/officepilot/，
# 由外壳作为后台进程启动。未签名；签名与公证在发布流程里统一处理。
# 旧版 Tauri 桌面版（desktop/）仍可用 `cd desktop/src-tauri && cargo tauri build` 单独打包。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[ "$(uname)" = "Darwin" ] || { echo "目前只支持在 macOS 上打包" >&2; exit 1; }

# 服务端内嵌前端（rust-embed），先构建前端
(cd "$ROOT/frontend" && npm run build)
(cd "$ROOT/server" && cargo build --release)
(cd "$ROOT/cli" && cargo build --release)

BIN="$(mktemp -d)"
cp "$ROOT/server/target/release/walioffice" "$BIN/"
cp "$ROOT/cli/target/release/officepilot" "$BIN/"

OFFICEPILOT_BIN_DIR="$BIN" SUITE_TARGETS="${SUITE_TARGETS:-dmg}" "$ROOT/scripts/build-suite.sh"
rm -rf "$BIN"

APP="$ROOT/suite/apps/shell/release/mac-arm64/OfficePilot.app"
[ -x "$APP/Contents/Resources/officepilot/walioffice" ] || { echo "安装包里没有 OfficePilot 服务" >&2; exit 1; }
echo "OfficePilot.app: $(du -sh "$APP" | cut -f1)"
ls -1 "$ROOT"/suite/apps/shell/release/*.dmg 2>/dev/null | while read -r dmg; do echo "dmg: $dmg（$(du -sh "$dmg" | cut -f1)）"; done
