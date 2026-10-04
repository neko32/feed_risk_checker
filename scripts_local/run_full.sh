#!/usr/bin/env bash
# 本番ボリューム実行スクリプト（Linux / bash）。
# 100,000件 / 1,000ユーザでfeed_risk_checkerを実行する。
#
# HITL（Human-In-The-Loop）ゲート: 対話的な確認プロンプトが表示される。
# 事前に小規模テスト（run_small.sh）で結果を確認してから実行することを推奨する。
# CI等で確認をスキップしたい場合は -y / --yes を渡す。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="${SCRIPT_DIR}/../app"

cd "${APP_DIR}"
echo "=== feed_risk_checker: 本番ボリューム実行 (100,000件 / 1,000ユーザ) ==="
cargo run --release -- run --scale full "$@"
