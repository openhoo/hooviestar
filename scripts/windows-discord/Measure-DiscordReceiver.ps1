[CmdletBinding()]
param(
    [ValidateRange(32, 3600)]
    [int]$DurationSeconds = 96,

    [string]$WindowTitleContains = "Discord",

    [string]$ReportPath,

    [ValidatePattern("^[A-Za-z0-9._-]+$")]
    [string]$RunId,

    [ValidatePattern("^[A-Za-z0-9._-]+$")]
    [string]$TransportName = "Discord"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$qualificationRoot = Join-Path $repo "target\windows-discord"
if (-not $ReportPath) {
    $ReportPath = Join-Path $qualificationRoot "discord-receiver.json"
}
New-Item -ItemType Directory -Force -Path $qualificationRoot | Out-Null
if (-not $RunId) {
    $activeRunPath = Join-Path $qualificationRoot "active-run.json"
    if (-not (Test-Path $activeRunPath)) { throw "No active qualification run. Pass -RunId." }
    $RunId = [string](Get-Content $activeRunPath -Raw | ConvertFrom-Json).qualificationRunId
}

Push-Location $repo
try {
    cargo build -p hooviestar-engine --example qualify_discord_receiver
    if ($LASTEXITCODE -ne 0) { throw "Discord receiver probe did not build." }

    Write-Host "Open the received Hooviestar stream in Discord before measurement starts."
    Write-Host "Keep stream visible, unmuted, and large enough that colored stage marker is readable."
    $receiver = Join-Path $repo "target\debug\examples\qualify_discord_receiver.exe"
    & $receiver `
        --window-title-contains $WindowTitleContains `
        --transport $TransportName `
        --duration $DurationSeconds `
        --run-id $RunId `
        --report $ReportPath
    if ($LASTEXITCODE -ne 0) {
        throw "Discord receiver qualification failed. Inspect $ReportPath."
    }
    Write-Host "Discord receiver qualification passed: $ReportPath"
}
finally {
    Pop-Location
}
