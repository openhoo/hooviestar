[CmdletBinding()]
param(
    [ValidateRange(1, 120)]
    [int]$TimeoutSeconds = 30,

    [ValidatePattern('^[0-9a-fA-F]+$')]
    [string]$WindowHandleHex,

    [switch]$NotTopmost,

    [string]$WindowTitle,

    [switch]$Foreground
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class HooviestarQualificationWindow {
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left; public int Top; public int Right; public int Bottom; }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern IntPtr FindWindow(string className, string windowName);

    [DllImport("user32.dll")]
    public static extern bool SetWindowPos(
        IntPtr window,
        IntPtr insertAfter,
        int x,
        int y,
        int width,
        int height,
        uint flags
    );

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr window, out Rect rectangle);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);

    [DllImport("user32.dll")]
    public static extern bool IsIconic(IntPtr window);

    [DllImport("user32.dll")]
    public static extern bool ShowWindowAsync(IntPtr window, int command);

    public delegate bool EnumWindowsCallback(IntPtr window, IntPtr parameter);

    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr parameter);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr window);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowTextLength(IntPtr window);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr window, StringBuilder text, int count);

    public static IntPtr FindVisibleWindowContaining(string fragment) {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate (IntPtr candidate, IntPtr parameter) {
            if (!IsWindowVisible(candidate)) return true;
            int length = GetWindowTextLength(candidate);
            if (length <= 0) return true;
            StringBuilder title = new StringBuilder(length + 1);
            GetWindowText(candidate, title, title.Capacity);
            if (title.ToString().IndexOf(fragment, StringComparison.OrdinalIgnoreCase) < 0) {
                return true;
            }
            found = candidate;
            return false;
        }, IntPtr.Zero);
        return found;
    }
}
"@

$programTitle = "Hooviestar " + [char]0x2013 + " Program"
$title = if ($WindowTitle) { $WindowTitle } else { $programTitle }
$window = [IntPtr]::Zero
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$diagnostic = Join-Path $repo "target\debug\examples\diagnose_windows_sources.exe"
if (-not (Test-Path $diagnostic)) {
    throw "Build diagnose_windows_sources before mapping Program."
}
if ($WindowHandleHex) {
    $window = [IntPtr]::new([Convert]::ToInt64($WindowHandleHex, 16))
} else {
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        # FindWindow is exact, session-local, and avoids starting the broad
        # source diagnostic while the trusted share picker is open. Use the
        # diagnostic only as a fallback for unusual title discovery failures.
        $window = [HooviestarQualificationWindow]::FindWindow($null, $title)
        if ($window -eq [IntPtr]::Zero -and $WindowTitle) {
            $window = [HooviestarQualificationWindow]::FindVisibleWindowContaining($WindowTitle)
        }
        if ($window -eq [IntPtr]::Zero -and $title -eq $programTitle) {
            $snapshot = (& $diagnostic 2>$null | Out-String | ConvertFrom-Json)
            $candidate = $snapshot.windows | Where-Object { $_.name -eq $title } | Select-Object -First 1
            if ($candidate -and $candidate.runtimeId -match '^hwnd:([0-9a-fA-F]+)$') {
                $window = [IntPtr]::new([Convert]::ToInt64($Matches[1], 16))
            }
        }
        if ($window -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)
}

if ($window -eq [IntPtr]::Zero) {
    throw "Program window did not appear within $TimeoutSeconds seconds."
}

$insertAfter = if ($NotTopmost) { [IntPtr]::new(-2) } else { [IntPtr]::new(-1) }
if ([HooviestarQualificationWindow]::IsIconic($window)) {
    # The return value reports the previous visibility state, not operation success.
    [void][HooviestarQualificationWindow]::ShowWindowAsync($window, 9)
    Start-Sleep -Milliseconds 250
}
$showWindow = [uint32]0x0040
if (-not [HooviestarQualificationWindow]::SetWindowPos($window, $insertAfter, 0, 0, 1280, 720, $showWindow)) {
    throw "SetWindowPos failed."
}
if ($Foreground -and -not [HooviestarQualificationWindow]::SetForegroundWindow($window)) {
    throw "SetForegroundWindow failed."
}

$rectangleDeadline = (Get-Date).AddSeconds([Math]::Min($TimeoutSeconds, 10))
do {
    $rectangle = New-Object HooviestarQualificationWindow+Rect
    if (-not [HooviestarQualificationWindow]::GetWindowRect($window, [ref]$rectangle)) {
        throw "GetWindowRect failed."
    }
    if ($rectangle.Left -ge -1000 -and $rectangle.Top -ge -1000 -and
        ($rectangle.Right - $rectangle.Left) -eq 1280 -and
        ($rectangle.Bottom - $rectangle.Top) -eq 720) {
        break
    }
    Start-Sleep -Milliseconds 100
} while ((Get-Date) -lt $rectangleDeadline)
if ($rectangle.Left -lt -1000 -or $rectangle.Top -lt -1000 -or
    ($rectangle.Right - $rectangle.Left) -ne 1280 -or
    ($rectangle.Bottom - $rectangle.Top) -ne 720) {
    throw "Program window did not restore to the expected 1280x720 onscreen rectangle."
}

[ordered]@{
    title = $title
    left = $rectangle.Left
    top = $rectangle.Top
    right = $rectangle.Right
    bottom = $rectangle.Bottom
    topmost = -not $NotTopmost
} | ConvertTo-Json
