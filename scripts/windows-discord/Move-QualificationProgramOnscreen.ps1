[CmdletBinding()]
param(
    [ValidateRange(1, 120)]
    [int]$TimeoutSeconds = 30
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type @"
using System;
using System.Runtime.InteropServices;

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
}
"@

$title = "Hooviestar " + [char]0x2013 + " Program"
$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
do {
    $window = [HooviestarQualificationWindow]::FindWindow($null, $title)
    if ($window -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 250
} while ((Get-Date) -lt $deadline)

if ($window -eq [IntPtr]::Zero) {
    throw "Program window did not appear within $TimeoutSeconds seconds."
}

$topmost = [IntPtr]::new(-1)
$showWindow = [uint32]0x0040
if (-not [HooviestarQualificationWindow]::SetWindowPos($window, $topmost, 0, 0, 1280, 720, $showWindow)) {
    throw "SetWindowPos failed."
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
    topmost = $true
} | ConvertTo-Json
