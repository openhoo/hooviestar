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
$showWindowAsync = [uint32](0x0040 -bor 0x4000)
if (-not [HooviestarQualificationWindow]::SetWindowPos($window, $insertAfter, 0, 0, 1280, 720, $showWindowAsync)) {
    throw "SetWindowPos failed."
}
if ($Foreground -and -not [HooviestarQualificationWindow]::SetForegroundWindow($window)) {
    throw "SetForegroundWindow failed."
}

$rectangle = New-Object HooviestarQualificationWindow+Rect
if (-not [HooviestarQualificationWindow]::GetWindowRect($window, [ref]$rectangle)) {
    throw "GetWindowRect failed."
}

[ordered]@{
    title = $title
    left = $rectangle.Left
    top = $rectangle.Top
    right = $rectangle.Right
    bottom = $rectangle.Bottom
    topmost = -not $NotTopmost
} | ConvertTo-Json
