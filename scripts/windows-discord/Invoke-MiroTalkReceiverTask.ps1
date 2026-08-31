[CmdletBinding()]
param(
    [ValidateRange(32, 3600)]
    [int]$DurationSeconds = 96,

    [string]$RepoPath,

    [ValidatePattern("^[A-Za-z0-9._-]+$")]
    [string]$RunId
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not $RepoPath) {
    $RepoPath = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}
$resultRoot = Join-Path $RepoPath "target\windows-discord"
$report = Join-Path $resultRoot "mirotalk-receiver.json"
$status = Join-Path $resultRoot "mirotalk-receiver-task-status.json"
$log = Join-Path $resultRoot "mirotalk-receiver-task.log"
$errorLog = Join-Path $resultRoot "mirotalk-receiver-task.stderr.log"
New-Item -ItemType Directory -Force -Path $resultRoot | Out-Null
if (-not $RunId) {
    $activeRunPath = Join-Path $resultRoot "active-run.json"
    if (-not (Test-Path $activeRunPath)) { throw "No active qualification run. Pass -RunId." }
    $RunId = [string](Get-Content $activeRunPath -Raw | ConvertFrom-Json).qualificationRunId
}

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
    $measureScript = Join-Path $RepoPath "scripts\windows-discord\Measure-DiscordReceiver.ps1"
    $receiver = Start-Process `
        -FilePath "powershell.exe" `
        -ArgumentList @(
            "-NoProfile",
            "-ExecutionPolicy", "Bypass",
            "-File", $measureScript,
            "-DurationSeconds", $DurationSeconds,
            "-WindowTitleContains", '"Hooviestar MiroTalk Receiver"',
            "-TransportName", "MiroTalk",
            "-RunId", $RunId,
            "-ReportPath", $report
        ) `
        -RedirectStandardOutput $log `
        -RedirectStandardError $errorLog `
        -Wait `
        -PassThru
    $exitCode = $receiver.ExitCode
    if ($exitCode -ne 0) { throw "MiroTalk receiver returned exit $exitCode" }
    if (-not (Test-Path $report)) { throw "MiroTalk receiver report is missing: $report" }
    $reportDocument = Get-Content $report -Raw | ConvertFrom-Json
    $reportPassed = $reportDocument.passed -eq $true
    $reportRunId = [string]$reportDocument.qualificationRunId
    $reportFresh = (Get-Item $report).LastWriteTimeUtc -ge $started.ToUniversalTime()
    if (-not $reportPassed) { throw "MiroTalk receiver report did not pass." }
    if ($reportRunId -ne $RunId) { throw "MiroTalk receiver report run ID mismatch." }
    if (-not $reportFresh) { throw "MiroTalk receiver report predates this task." }
}
catch {
    $failure = $_.Exception.Message
    $_ | Out-String | Add-Content $errorLog
    if ($exitCode -eq 0) { $exitCode = 1 }
}
finally {
    [ordered]@{
        exitCode = $exitCode
        qualificationRunId = $RunId
        started = $started.ToString("o")
        completed = (Get-Date).ToString("o")
        durationSeconds = $DurationSeconds
        reportExists = (Test-Path $report)
        reportPassed = $reportPassed
        reportFresh = $reportFresh
        reportRunId = $reportRunId
        reportPath = $report
        logPath = $log
        errorLogPath = $errorLog
        failure = $failure
    } | ConvertTo-Json | Set-Content $status -Encoding UTF8
}

exit $exitCode
