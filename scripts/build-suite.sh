#!/usr/bin/env bash
# 构建编辑器套件（suite/ 子模块）的 macOS .app，供 OfficePilot 桌面版打包或开发时直接调用。
#
#   scripts/build-suite.sh            # 产物：suite/apps/shell/release/mac-arm64/OfficePilot Suite.app
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
CSC_IDENTITY_AUTO_DISCOVERY=false npx electron-builder --mac --dir --arm64

APP="$SUITE/apps/shell/release/mac-arm64/OfficePilot Suite.app"
[ -d "$APP" ] || { echo "没有找到构建产物：$APP" >&2; exit 1; }
echo "$APP"
