# 本番ボリューム実行スクリプト（Windows / PowerShell）。
# 100,000件 / 1,000ユーザでfeed_risk_checkerを実行する。
#
# HITL（Human-In-The-Loop）ゲート: 対話的な確認プロンプトが表示される。
# 事前に小規模テスト（run_small.ps1）で結果を確認してから実行することを推奨する。
# CI等で確認をスキップしたい場合は -y を付ける。

param(
    [switch]$Yes
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$AppDir = Join-Path $ScriptDir "..\app"

Push-Location $AppDir
try {
    Write-Host "=== feed_risk_checker: 本番ボリューム実行 (100,000件 / 1,000ユーザ) ===" -ForegroundColor Yellow
    if ($Yes) {
        cargo run --release -- run --scale full --yes
    }
    else {
        cargo run --release -- run --scale full
    }
}
finally {
    Pop-Location
}
