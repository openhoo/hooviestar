[CmdletBinding()]
param(
    [ValidateRange(0, 3600)]
    [int]$HoldSeconds = 180,

    [string]$ReportPath,

    [switch]$NativeOnly,

    [ValidatePattern("^[A-Za-z0-9._-]+$")]
    [string]$TransportName = "Discord"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$qualificationRoot = Join-Path $repo "target\windows-discord"
$runRoot = Join-Path $qualificationRoot ("run-" + [Guid]::NewGuid().ToString("N"))
$browserProfile = Join-Path $runRoot "browser-profile"
$appData = Join-Path $runRoot "publisher-appdata"
$fixture = (Resolve-Path (Join-Path $repo "tests\windows-discord\browser-video-fixture.html")).Path
if (-not $ReportPath) {
    $ReportPath = Join-Path $qualificationRoot "publisher-native.json"
}

New-Item -ItemType Directory -Force -Path $qualificationRoot, $runRoot, $browserProfile, $appData | Out-Null

$browserCandidates = @(
    (Join-Path ${env:ProgramFiles(x86)} "Microsoft\Edge\Application\msedge.exe"),
    (Join-Path $env:ProgramFiles "Microsoft\Edge\Application\msedge.exe"),
    (Join-Path $env:LocalAppData "Microsoft\Edge\Application\msedge.exe"),
    (Join-Path $env:ProgramFiles "Google\Chrome\Application\chrome.exe"),
    (Join-Path ${env:ProgramFiles(x86)} "Google\Chrome\Application\chrome.exe")
)
$browserExecutable = $browserCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $browserExecutable) {
    throw "Microsoft Edge or Google Chrome is required for the browser <video> fixture."
}
if (
    -not $NativeOnly -and
    $TransportName -eq "Discord" -and
    -not (Get-Process -Name Discord -ErrorAction SilentlyContinue)
) {
    throw "Discord desktop must already be running and signed in. Use -NativeOnly to skip the Discord hold."
}

Push-Location $repo
$tone = $null
$browser = $null
try {
    cargo build -p hooviestar-engine --example tone_session --example qualify_windows_pipeline
    if ($LASTEXITCODE -ne 0) { throw "Windows qualification examples did not build." }

    $fixtureUri = [Uri]::new($fixture).AbsoluteUri
    $browserArguments = @(
        "--user-data-dir=`"$browserProfile`"",
        "--autoplay-policy=no-user-gesture-required",
        "--disable-background-timer-throttling",
        "--disable-renderer-backgrounding",
        "--app=`"$fixtureUri`""
    ) -join " "
    $browser = Start-Process `
        -FilePath $browserExecutable `
        -ArgumentList $browserArguments `
        -RedirectStandardOutput (Join-Path $qualificationRoot "browser-fixture.stdout.log") `
        -RedirectStandardError (Join-Path $qualificationRoot "browser-fixture.stderr.log") `
        -PassThru

    $toneExecutable = Join-Path $repo "target\debug\examples\tone_session.exe"
    $toneDuration = [Math]::Max($HoldSeconds + 180, 300)
    $tone = Start-Process `
        -FilePath $toneExecutable `
        -ArgumentList @(
            "--frequency", "440",
            "--amplitude", "0.20",
            "--duration", $toneDuration,
            "--grouping", "48564f4f-5649-4553-5441-52544f4e4502"
        ) `
        -RedirectStandardOutput (Join-Path $qualificationRoot "tone-session.stdout.log") `
        -RedirectStandardError (Join-Path $qualificationRoot "tone-session.stderr.log") `
        -PassThru

    $effectiveHold = if ($NativeOnly) { 0 } else { $HoldSeconds }
    if (-not $NativeOnly) {
        Write-Host "Share 'Hooviestar - Program' through $TransportName with sound enabled."
        Write-Host "Keep browser fixture visible. Receiver must keep the transported stream visible and audible."
    }

    $publisherExecutable = Join-Path $repo "target\debug\examples\qualify_windows_pipeline.exe"
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $publisherExecutable
    $startInfo.UseShellExecute = $false
    $publisherArguments = "--tone-pid $($tone.Id) --hold-seconds $effectiveHold --transport-label $TransportName --report `"$ReportPath`""
    if ($TransportName -eq "MiroTalk" -and -not $NativeOnly) {
        $topmostGate = Join-Path $qualificationRoot "share-picker-open.gate"
        $publisherArguments += " --program-onscreen --system-audio-transport --program-topmost-gate `"$topmostGate`""
    }
    $startInfo.Arguments = $publisherArguments
    $startInfo.EnvironmentVariables["APPDATA"] = $appData
    $startInfo.EnvironmentVariables["HOOVIESTAR_QUALIFICATION_FAILURE_FRAME"] = Join-Path $qualificationRoot "last-failed-frame.ppm"
    $publisher = [System.Diagnostics.Process]::Start($startInfo)
    $publisher.WaitForExit()
    if ($publisher.ExitCode -ne 0) {
        throw "Publisher qualification failed with exit code $($publisher.ExitCode). Inspect $ReportPath."
    }
    Write-Host "Publisher qualification passed: $ReportPath"
}
finally {
    if ($tone -and -not $tone.HasExited) {
        Stop-Process -Id $tone.Id -Force -ErrorAction SilentlyContinue
    }
    # Only stop browser processes carrying this qualification's unique profile.
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains($browserProfile) } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    # Chromium can leave cache journal handles pending after its process tree
    # exits. Recursive deletion has been observed to block Windows PowerShell
    # indefinitely, hiding an otherwise clean qualification exit. Keep the
    # isolated profile under ignored target output for later evidence cleanup.
    Pop-Location
}
