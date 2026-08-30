[CmdletBinding()]
param(
    [ValidateRange(32, 3600)]
    [int]$DurationSeconds = 96,

    [string]$RepoPath
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

$started = Get-Date
$exitCode = 1
$failure = $null
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
            "-ReportPath", $report
        ) `
        -RedirectStandardOutput $log `
        -RedirectStandardError $errorLog `
        -Wait `
        -PassThru
    $exitCode = $receiver.ExitCode
    if ($exitCode -ne 0) { throw "MiroTalk receiver returned exit $exitCode" }
}
catch {
    $failure = $_.Exception.Message
    $_ | Out-String | Add-Content $errorLog
    if ($exitCode -eq 0) { $exitCode = 1 }
}
finally {
    [ordered]@{
        exitCode = $exitCode
        started = $started.ToString("o")
        completed = (Get-Date).ToString("o")
        durationSeconds = $DurationSeconds
        reportExists = (Test-Path $report)
        reportPath = $report
        logPath = $log
        errorLogPath = $errorLog
        failure = $failure
    } | ConvertTo-Json | Set-Content $status -Encoding UTF8
}

exit $exitCode
