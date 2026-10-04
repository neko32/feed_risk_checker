#!/usr/bin/env bash
# 結果ビューワー起動スクリプト（Linux / bash）。
# ふっかちゃん製のTKInterビューワーを起動する。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VIEWER_PATH="${SCRIPT_DIR}/../viewer/viewer.py"
DEFAULT_DB="${SCRIPT_DIR}/../app/feed_risk.db"

python3 "${VIEWER_PATH}" "${1:-$DEFAULT_DB}"
