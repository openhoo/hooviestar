[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("Publisher", "Receiver")]
    [string]$Role,

    [string]$Room = "hooviestar-e2e",

    [ValidateRange(1024, 65535)]
    [int]$CdpPort,

    [string]$ServerOrigin = "http://127.0.0.1:3016"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not $CdpPort) {
    $CdpPort = if ($Role -eq "Publisher") { 9223 } else { 9224 }
}

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$resultRoot = Join-Path $repo "target\windows-discord\mirotalk"
$profile = Join-Path $resultRoot ($Role.ToLowerInvariant() + "-profile")
$statusPath = Join-Path $resultRoot ($Role.ToLowerInvariant() + "-client.json")
New-Item -ItemType Directory -Force -Path $resultRoot, $profile | Out-Null

$browserCandidates = @(
    (Join-Path ${env:ProgramFiles(x86)} "Microsoft\Edge\Application\msedge.exe"),
    (Join-Path $env:ProgramFiles "Microsoft\Edge\Application\msedge.exe"),
    (Join-Path $env:LocalAppData "Microsoft\Edge\Application\msedge.exe")
)
$browserExecutable = $browserCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browserExecutable) {
    throw "Microsoft Edge is required for MiroTalk qualification."
}

# Stop only a prior Edge tree carrying this qualification profile.
Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -and $_.CommandLine.Contains($profile) } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

$encodedRoom = [Uri]::EscapeDataString($Room)
$encodedName = [Uri]::EscapeDataString(("Hooviestar " + $Role))
$page = if ($Role -eq "Publisher") { "broadcast" } else { "viewer" }
$url = "$ServerOrigin/views/$page.html?id=$encodedRoom&name=$encodedName"
$arguments = @(
    "--user-data-dir=`"$profile`"",
    "--remote-debugging-address=127.0.0.1",
    "--remote-debugging-port=$CdpPort",
    "--autoplay-policy=no-user-gesture-required",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-timer-throttling",
    "--disable-renderer-backgrounding"
)
if ($Role -eq "Publisher") {
    # Browser WebRTC cannot isolate a native application's audio. Capture the
    # qualification monitor and map Program onto it for the transport stage.
    $arguments += '--auto-select-desktop-capture-source="Entire screen"'
}
$arguments += "--app=`"$url`""

$process = Start-Process -FilePath $browserExecutable -ArgumentList ($arguments -join " ") -PassThru
[ordered]@{
    role = $Role
    processId = $process.Id
    cdpPort = $CdpPort
    profile = $profile
    url = $url
    started = (Get-Date).ToString("o")
} | ConvertTo-Json | Set-Content $statusPath -Encoding UTF8

Write-Host "$Role MiroTalk client started on CDP port $CdpPort"
