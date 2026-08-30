# Windows and Discord qualification

This qualification uses real Windows capture/audio APIs and an actual Discord screen share. It has two measured layers:

1. Publisher native qualification proves Hooviestar before transport.
2. Receiver qualification proves motion, scene markers, and mixed sound after Discord encoding and playback.

Discord login and starting/viewing the share remain operator actions. No account credentials, user tokens, or unstable private Discord APIs enter the harness.

## What is measured

Publisher probe fails unless all checks pass:

- **Hooviestar – Program** stays mapped, non-minimized, fully outside the virtual desktop, and capturable through Windows Graphics Capture.
- Browser fixture runs a real autoplaying `<video>` backed by a 30 fps `MediaStream`, with a 660 Hz audio track.
- Four scenes switch to distinct magenta, cyan, yellow, and blue render markers.
- Browser video keeps changing through full-screen, cropped, and picture-in-picture transforms.
- Program and Preview both deliver D3D11-composited frames.
- Renderer keeps delivering output after 1280×720/30 to 1920×1080/60 reconfiguration and back.
- WASAPI process loopback captures browser audio and an independent 440 Hz process.
- Per-source volume and mute isolate both tones, mixed stage contains both, output limiter stays below its ceiling, and muted stage approaches silence.

Receiver probe watches the visible Discord client window and captures its process audio. It recognizes the same four color markers, measures motion during every video scene, and runs frequency analysis on received 660 Hz/440 Hz audio. This makes a passing report evidence of Discord transport, not only local rendering.

## Requirements

- Windows 11 in an interactive logged-in desktop session.
- Hardware D3D11 device and working default playback endpoint.
- Microsoft Edge or Google Chrome.
- Discord desktop, signed in and joined to a voice channel.
- For full transport proof, a second Discord account in the same voice channel. It may run on a second Windows desktop, or in regular Microsoft Edge on the same qualification VM while Discord desktop publishes.
- Rust toolchain matching this repository.

Do not run the live qualification as a service or ordinary hosted CI job. Windows Graphics Capture, Discord, and WASAPI need the interactive user desktop and audio endpoint. Existing Windows CI still compiles the probes and runs deterministic Windows unit tests.

## 1. Native publisher qualification

No Discord required:

```powershell
pwsh -File .\scripts\windows-discord\Start-Publisher.ps1 -NativeOnly
```

Result: `target\windows-discord\publisher-native.json`. Exit code is non-zero if any scene, frame-motion, output-resize, audio-isolation, mix, mute, or limiter assertion fails.

## 2. Real Discord publisher

Start Discord first. Then on publisher host:

```powershell
pwsh -File .\scripts\windows-discord\Start-Publisher.ps1 -HoldSeconds 300
```

In Discord:

1. Open **Share Your Screen**.
2. Select **Hooviestar – Program** under **Applications**.
3. Enable application sound.
4. Start stream at 720p/30 or better.

Program window intentionally never appears on a monitor. It remains mapped offscreen so Discord can enumerate and capture it. Publisher cycles four eight-second stages after native checks pass:

- magenta: browser video plus 660 Hz browser audio;
- cyan: transformed browser video plus 440 Hz independent tone;
- yellow: browser picture-in-picture plus both sources at 50%;
- blue: muted audio reference.

## 3. Discord receiver measurement

On receiver host, open the publisher stream inside Discord, keep it visible, maximize the stream area, and leave stream sound unmuted. Then run:

```powershell
pwsh -File .\scripts\windows-discord\Measure-DiscordReceiver.ps1 -DurationSeconds 96
```

If another visible window also contains `Discord` in its title, pass a unique fragment:

```powershell
pwsh -File .\scripts\windows-discord\Measure-DiscordReceiver.ps1 `
  -WindowTitleContains "Discord | Hooviestar" `
  -DurationSeconds 120
```

Result: `target\windows-discord\discord-receiver.json`. Pass requires every stage marker, live motion in all three browser scenes, correct single-source isolation, both frequencies in the mixed stage, and a substantially quieter muted stage.

Keep voice chat quiet and disable unrelated notification sounds during receiver measurement. The receiver intentionally captures the Discord process tree, so unrelated Discord sounds correctly count as transport noise and can fail mute/isolation thresholds.

## Reading failures

- Missing marker: wrong shared window, stream not visible on receiver, or frozen Program output.
- Low `maximumMotionRatio`: browser video stopped, Windows capture stalled, or Discord delivered frozen frames.
- Missing 660 Hz: browser autoplay/audio session unavailable, browser audio source failed, or Discord share sound disabled.
- Missing 440 Hz: independent application-audio capture or mix failed.
- High cross-frequency leakage in a single-source stage: mute/volume command did not reach mixer or stale audio was transmitted.
- High muted RMS: unrelated Discord audio, source-session leak, or mixer mute failure.
- Publisher passes but receiver fails: local Hooviestar path works; failure lies in Discord selection, share-sound configuration, transport, or receiver playback/capture.

Both JSON files are required evidence for a full Windows-with-Discord qualification. A green CI run alone is not equivalent.

## One-VM transport layout

The qualification VM can test both Discord endpoints without cloning a second guest:

1. Sign the publisher account into Discord desktop.
2. Sign a second account into `https://discord.com/app` in regular Microsoft Edge.
3. Join both accounts to the same voice channel.
4. Run `Invoke-DiscordPublisherTask.ps1` as an interactive-token scheduled task.
5. Share **Hooviestar - Program** from Discord desktop with application sound enabled.
6. Open and maximize the received stream in Edge.
7. Run `Invoke-DiscordReceiverTask.ps1`; its default title fragment is `Microsoft Edge` so it captures the receiver, not the desktop publisher.

The browser-video fixture uses a separate temporary Edge profile. Close other regular Edge windows so the receiver title remains unique.

Qualification run profiles remain under ignored `target\windows-discord\run-*` output. Edge can retain cache-journal handles after its process tree exits; eager recursive deletion can block Windows PowerShell and hide a clean process exit. Remove old profiles only outside an active qualification run.

Windows process-loopback is post-session-mute on current Windows 11: muting the original Edge/tone session also silences Hooviestar's capture input. Hooviestar therefore leaves source sessions audible and renders its mixed result through its own process session. Discord application sharing captures that Hooviestar session. Local speaker monitoring can contain both the original sessions and the mixed session; the receiver oracle remains process-isolated and measures only the transported Edge playback.
