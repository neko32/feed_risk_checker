# 結果ビューワー起動スクリプト（Windows / PowerShell）。
# ふっかちゃん製のTKInterビューワーを起動する。

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ViewerPath = Join-Path $ScriptDir "..\viewer\viewer.py"
$DefaultDb = Join-Path $ScriptDir "..\app\feed_risk.db"

if ($args.Count -gt 0) {
    python $ViewerPath $args[0]
}
else {
    python $ViewerPath $DefaultDb
}
