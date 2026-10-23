#!/usr/bin/env bash
# 构建编辑器套件外壳（suite/ 子模块）的 macOS .app。
#
#   scripts/build-suite.sh                    # 只有编辑器：suite/apps/shell/release/mac-arm64/OfficePilot.app
#   OFFICEPILOT_BIN_DIR=<目录> scripts/build-suite.sh   # 连同 OfficePilot 服务（scripts/build-desktop.sh 这样调用）
#   SUITE_TARGETS="dmg" scripts/build-suite.sh          # 另外打 dmg（默认只出 .app 目录）
#
# 不签名（CSC_IDENTITY_AUTO_DISCOVERY=false）；签名与公证在发布流程里统一处理。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SUITE="$ROOT/suite"

if [ ! -f "$SUITE/package.json" ]; then
  echo "suite/ 子模块未初始化：git submodule update --init suite" >&2
  exit 1
fi
command -v node >/dev/null || { echo "需要 Node.js 22（PATH 里没有 node）" >&2; exit 1; }
command -v cargo >/dev/null || { echo "需要 Rust（表格引擎 sidecar）" >&2; exit 1; }

cd "$SUITE"
[ -d node_modules ] || npm ci
npm run notices
npm run build:all
# 表格的原生引擎（宿主架构）
npm run native:build -w @genoffice/sheets
cd apps/shell
rm -rf release/mac-arm64
# shellcheck disable=SC2086
CSC_IDENTITY_AUTO_DISCOVERY=false npx electron-builder --mac ${SUITE_TARGETS:-dir} --arm64

APP="$SUITE/apps/shell/release/mac-arm64/OfficePilot.app"
[ -d "$APP" ] || { echo "没有找到构建产物：$APP" >&2; exit 1; }
echo "$APP"
