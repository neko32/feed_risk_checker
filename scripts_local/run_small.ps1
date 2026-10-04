# 小規模テスト実行スクリプト（Windows / PowerShell）。
# 1,000件 / 5ユーザでfeed_risk_checkerを実行する。
# LM Studioがローカルで起動していること（既定: http://localhost:1234）を前提とする。

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$AppDir = Join-Path $ScriptDir "..\app"

Push-Location $AppDir
try {
    Write-Host "=== feed_risk_checker: 小規模テスト実行 (1,000件 / 5ユーザ) ===" -ForegroundColor Cyan
    cargo run --release -- run --scale small
}
finally {
    Pop-Location
}
