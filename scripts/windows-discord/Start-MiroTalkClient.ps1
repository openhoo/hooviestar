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

function Stop-QualificationProcessTree {
    param([int[]]$RootProcessIds)
    if ($RootProcessIds.Count -eq 0) { return }
    $snapshot = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)
    $targetIds = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($processId in $RootProcessIds) { [void]$targetIds.Add($processId) }
    do {
        $added = $false
        foreach ($candidate in $snapshot) {
            if ($targetIds.Contains([int]$candidate.ParentProcessId) -and
                $targetIds.Add([int]$candidate.ProcessId)) {
                $added = $true
            }
        }
    } while ($added)
    foreach ($processId in $targetIds) {
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
    }
}

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

# Stop only a prior Edge tree carrying this qualification profile. Killing the
# full tree and waiting for the CDP port prevents a rapid restart from falling
# back to an IPv6-only loopback listener while the old IPv4 socket still exits.
$priorRoots = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -and $_.CommandLine.Contains($profile) }
Stop-QualificationProcessTree @($priorRoots | ForEach-Object { [int]$_.ProcessId })
$releaseDeadline = (Get-Date).AddSeconds(15)
do {
    $listeners = @(Get-NetTCPConnection -State Listen -LocalPort $CdpPort -ErrorAction SilentlyContinue)
    if ($listeners.Count -eq 0) { break }
    Start-Sleep -Milliseconds 250
} while ((Get-Date) -lt $releaseDeadline)
if ($listeners.Count -ne 0) {
    throw "CDP port $CdpPort remained occupied after stopping the prior $Role client."
}

$encodedRoom = [Uri]::EscapeDataString($Room)
$encodedName = [Uri]::EscapeDataString(("Hooviestar " + $Role))
$page = if ($Role -eq "Publisher") { "broadcast" } else { "viewer" }
$url = "$ServerOrigin/views/$page.html?id=$encodedRoom&name=$encodedName"
$autoplayPolicy = if ($Role -eq "Receiver") {
    "document-user-activation-required"
} else {
    "no-user-gesture-required"
}
$arguments = @(
    "--user-data-dir=`"$profile`"",
    "--remote-debugging-address=127.0.0.1",
    "--remote-debugging-port=$CdpPort",
    "--autoplay-policy=$autoplayPolicy",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-timer-throttling",
    "--disable-renderer-backgrounding",
    "--disable-backgrounding-occluded-windows",
    "--disable-features=CalculateNativeWinOcclusion"
)
$arguments += "--app=`"$url`""

$process = Start-Process -FilePath $browserExecutable -ArgumentList ($arguments -join " ") -PassThru
$cdpReady = $false
$cdpDeadline = (Get-Date).AddSeconds(30)
do {
    try {
        $version = Invoke-RestMethod -Uri "http://127.0.0.1:$CdpPort/json/version" -TimeoutSec 2
        $cdpReady = [bool]$version.webSocketDebuggerUrl
    }
    catch {
        $cdpReady = $false
    }
    if ($cdpReady) { break }
    if ($process.HasExited) { throw "$Role browser exited before CDP became ready." }
    Start-Sleep -Milliseconds 250
} while ((Get-Date) -lt $cdpDeadline)
if (-not $cdpReady) {
    Stop-QualificationProcessTree @([int]$process.Id)
    throw "$Role CDP did not become reachable on IPv4 loopback port $CdpPort."
}
[ordered]@{
    role = $Role
    processId = $process.Id
    cdpPort = $CdpPort
    profile = $profile
    url = $url
    cdpReady = $cdpReady
    started = (Get-Date).ToString("o")
} | ConvertTo-Json | Set-Content $statusPath -Encoding UTF8

Write-Host "$Role MiroTalk client started on CDP port $CdpPort"
