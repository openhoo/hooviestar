[CmdletBinding()]
param(
    [ValidateRange(32, 3600)]
    [int]$HoldSeconds = 180,

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
$report = Join-Path $resultRoot "publisher-mirotalk-native.json"
$status = Join-Path $resultRoot "publisher-mirotalk-task-status.json"
$log = Join-Path $resultRoot "publisher-mirotalk-task.log"
$errorLog = Join-Path $resultRoot "publisher-mirotalk-task.stderr.log"
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
            "-TransportName", "MiroTalk",
            "-RunId", $RunId,
            "-ReportPath", $report
        ) `
        -RedirectStandardOutput $log `
        -RedirectStandardError $errorLog `
        -Wait `
        -PassThru
    $exitCode = $publisher.ExitCode
    if ($exitCode -ne 0) { throw "MiroTalk publisher returned exit $exitCode" }
    if (-not (Test-Path $report)) { throw "MiroTalk publisher report is missing: $report" }
    $reportDocument = Get-Content $report -Raw | ConvertFrom-Json
    $reportPassed = $reportDocument.passed -eq $true
    $reportRunId = [string]$reportDocument.qualificationRunId
    $reportFresh = (Get-Item $report).LastWriteTimeUtc -ge $started.ToUniversalTime()
    if (-not $reportPassed) { throw "MiroTalk publisher report did not pass." }
    if ($reportRunId -ne $RunId) { throw "MiroTalk publisher report run ID mismatch." }
    if (-not $reportFresh) { throw "MiroTalk publisher report predates this task." }
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
        holdSeconds = $HoldSeconds
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
