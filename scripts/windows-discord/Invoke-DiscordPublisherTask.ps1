[CmdletBinding()]
param(
    [ValidateRange(32, 3600)]
    [int]$HoldSeconds = 300,

    [string]$RepoPath,

    [ValidatePattern("^[A-Za-z0-9._-]+$")]
    [string]$RunId,

    [string]$ReportPath,

    [string]$StatusPath,

    [string]$LogPath,

    [string]$ErrorLogPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not $RepoPath) {
    $RepoPath = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
$resultRoot = Join-Path $RepoPath "target\windows-discord"
if (-not $ReportPath) { $ReportPath = Join-Path $resultRoot "publisher-discord-native.json" }
if (-not $StatusPath) { $StatusPath = Join-Path $resultRoot "publisher-discord-task-status.json" }
if (-not $LogPath) { $LogPath = Join-Path $resultRoot "publisher-discord-task.log" }
if (-not $ErrorLogPath) { $ErrorLogPath = Join-Path $resultRoot "publisher-discord-task.stderr.log" }
New-Item -ItemType Directory -Force -Path $resultRoot | Out-Null
if (-not $RunId) { $RunId = [Guid]::NewGuid().ToString("D") }

$started = Get-Date
$exitCode = 1
$failure = $null
$reportPassed = $false
$reportFresh = $false
$reportRunId = $null
try {
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
    $env:CARGO_TERM_COLOR = "never"
    Import-Module "C:\BuildTools\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
    Enter-VsDevShell `
        -VsInstallPath "C:\BuildTools" `
        -SkipAutomaticLocation `
        -DevCmdArguments "-arch=x64 -host_arch=x64"
    $publisherScript = Join-Path $RepoPath "scripts\windows-discord\Start-Publisher.ps1"
    $publisher = Start-Process `
        -FilePath "powershell.exe" `
        -ArgumentList @(
            "-NoProfile",
            "-ExecutionPolicy", "Bypass",
            "-File", $publisherScript,
            "-HoldSeconds", $HoldSeconds,
            "-RunId", $RunId,
            "-ReportPath", $ReportPath
        ) `
        -RedirectStandardOutput $LogPath `
        -RedirectStandardError $ErrorLogPath `
        -Wait `
        -PassThru
    $exitCode = $publisher.ExitCode
    if ($exitCode -ne 0) { throw "Discord publisher returned exit $exitCode" }
    if (-not (Test-Path $ReportPath)) { throw "Discord publisher report is missing: $ReportPath" }
    $reportDocument = Get-Content $ReportPath -Raw | ConvertFrom-Json
    $reportPassed = $reportDocument.passed -eq $true
    $reportRunId = [string]$reportDocument.qualificationRunId
    $reportFresh = (Get-Item $ReportPath).LastWriteTimeUtc -ge $started.ToUniversalTime()
    if (-not $reportPassed) { throw "Discord publisher report did not pass." }
    if ($reportRunId -ne $RunId) { throw "Discord publisher report run ID mismatch." }
    if (-not $reportFresh) { throw "Discord publisher report predates this task." }
}
catch {
    $failure = $_.Exception.Message
    $_ | Out-String | Add-Content $ErrorLogPath
    if ($exitCode -eq 0) { $exitCode = 1 }
}
finally {
    [ordered]@{
        exitCode = $exitCode
        qualificationRunId = $RunId
        started = $started.ToString("o")
        completed = (Get-Date).ToString("o")
        holdSeconds = $HoldSeconds
        reportExists = (Test-Path $ReportPath)
        reportPassed = $reportPassed
        reportFresh = $reportFresh
        reportRunId = $reportRunId
        reportPath = $ReportPath
        logPath = $LogPath
        errorLogPath = $ErrorLogPath
        failure = $failure
    } | ConvertTo-Json | Set-Content $StatusPath -Encoding UTF8
}

exit $exitCode
