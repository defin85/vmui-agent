$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$logDir = Join-Path $repoRoot 'var\log'
$logFile = Join-Path $logDir 'vmui-agent.log'

New-Item -Force -ItemType Directory -Path $logDir | Out-Null
Set-Location -LiteralPath $repoRoot

$env:RUST_LOG = 'info'
"=== vmui-agent start $(Get-Date -Format o) ===" | Tee-Object -FilePath $logFile -Append | Out-Null
cargo run -p vmui-agent *>> $logFile
