#!/usr/bin/env bash
# 小規模テスト実行スクリプト（Linux / bash）。
# 1,000件 / 5ユーザでfeed_risk_checkerを実行する。
# LM Studioがローカルで起動していること（既定: http://localhost:1234）を前提とする。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="${SCRIPT_DIR}/../app"

cd "${APP_DIR}"
echo "=== feed_risk_checker: 小規模テスト実行 (1,000件 / 5ユーザ) ==="
cargo run --release -- run --scale small
