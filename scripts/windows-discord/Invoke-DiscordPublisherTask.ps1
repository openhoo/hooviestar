[CmdletBinding()]
param(
    [ValidateRange(32, 3600)]
    [int]$HoldSeconds = 300,

    [string]$RepoPath,

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
    $publisherScript = Join-Path $RepoPath "scripts\windows-discord\Start-Publisher.ps1"
    $publisher = Start-Process `
        -FilePath "powershell.exe" `
        -ArgumentList @(
            "-NoProfile",
            "-ExecutionPolicy", "Bypass",
            "-File", $publisherScript,
            "-HoldSeconds", $HoldSeconds,
            "-ReportPath", $ReportPath
        ) `
        -RedirectStandardOutput $LogPath `
        -RedirectStandardError $ErrorLogPath `
        -Wait `
        -PassThru
    $exitCode = $publisher.ExitCode
    if ($exitCode -ne 0) { throw "Discord publisher returned exit $exitCode" }
}
catch {
    $failure = $_.Exception.Message
    $_ | Out-String | Add-Content $ErrorLogPath
    if ($exitCode -eq 0) { $exitCode = 1 }
}
finally {
    [ordered]@{
        exitCode = $exitCode
        started = $started.ToString("o")
        completed = (Get-Date).ToString("o")
        holdSeconds = $HoldSeconds
        reportExists = (Test-Path $ReportPath)
        reportPath = $ReportPath
        logPath = $LogPath
        errorLogPath = $ErrorLogPath
        failure = $failure
    } | ConvertTo-Json | Set-Content $StatusPath -Encoding UTF8
}

exit $exitCode
