$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$logDir = Join-Path $repoRoot 'var\log'
$logFile = Join-Path $logDir 'vmui-smoke.log'

New-Item -Force -ItemType Directory -Path $logDir | Out-Null
Set-Location -LiteralPath $repoRoot

$report = [ordered]@{
    timestamp = Get-Date -Format o
    user = [Environment]::UserName
    host = $env:COMPUTERNAME
    session = $env:SESSIONNAME
    interactive = [Environment]::UserInteractive
    powershell = $PSVersionTable.PSVersion.ToString()
}

"=== vmui smoke $(Get-Date -Format o) ===" | Tee-Object -FilePath $logFile -Append | Out-Null
$report | ConvertTo-Json -Depth 4 | Tee-Object -FilePath $logFile -Append | Out-Null
