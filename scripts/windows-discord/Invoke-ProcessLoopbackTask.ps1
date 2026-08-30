[CmdletBinding()]
param(
    [string]$RepoPath,

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
if (-not $StatusPath) {
    $StatusPath = Join-Path $resultRoot "process-loopback-task-status.json"
}
if (-not $LogPath) {
    $LogPath = Join-Path $resultRoot "process-loopback-task.log"
}
if (-not $ErrorLogPath) {
    $ErrorLogPath = Join-Path $resultRoot "process-loopback-task.stderr.log"
}
New-Item -ItemType Directory -Force -Path $resultRoot | Out-Null

$tone = $null
$exitCode = 1
$failure = $null
$started = Get-Date
try {
    $examples = Join-Path $RepoPath "target\debug\examples"
    $toneExecutable = Join-Path $examples "tone_session.exe"
    $probeExecutable = Join-Path $examples "qualify_process_loopback.exe"
    if (-not (Test-Path $toneExecutable) -or -not (Test-Path $probeExecutable)) {
        throw "Build tone_session and qualify_process_loopback before starting interactive task."
    }
    $tone = Start-Process -FilePath $toneExecutable -ArgumentList @(
        "--frequency", "440",
        "--amplitude", "0.20",
        "--duration", "90",
        "--grouping", "48564f4f-5649-4553-5441-52544f4e4502"
    ) -PassThru
    Start-Sleep -Seconds 2
    $probe = Start-Process `
        -FilePath $probeExecutable `
        -RedirectStandardOutput $LogPath `
        -RedirectStandardError $ErrorLogPath `
        -Wait `
        -PassThru
    $exitCode = $probe.ExitCode
    if ($exitCode -ne 0) {
        throw "Process-loopback probe returned exit $exitCode"
    }
}
catch {
    $failure = $_.Exception.Message
    $_ | Out-String | Add-Content $ErrorLogPath
    if ($exitCode -eq 0) {
        $exitCode = 1
    }
}
finally {
    if ($tone -and -not $tone.HasExited) {
        Stop-Process -Id $tone.Id -Force -ErrorAction SilentlyContinue
    }
    [ordered]@{
        exitCode = $exitCode
        started = $started.ToString("o")
        completed = (Get-Date).ToString("o")
        logPath = $LogPath
        errorLogPath = $ErrorLogPath
        failure = $failure
    } | ConvertTo-Json | Set-Content $StatusPath -Encoding UTF8
}

exit $exitCode
