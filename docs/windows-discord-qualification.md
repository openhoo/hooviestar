# Windows screen-share qualification

This qualification uses real Windows capture/audio APIs and either local MiroTalk BRO or Discord screen sharing. It has two measured layers:

1. Publisher native qualification proves Hooviestar before transport.
2. Receiver qualification proves motion, scene markers, and mixed sound after WebRTC encoding and playback.

MiroTalk is the deterministic no-account default. Discord login and starting/viewing its share remain operator actions. No account credentials, user tokens, or unstable private Discord APIs enter the harness.

## What is measured

Publisher probe fails unless all checks pass:

- **Hooviestar – Program** stays mapped, non-minimized, offscreen for application-share discovery, and capturable through Windows Graphics Capture. Transport mode moves the already-qualified window to exactly 1280×720 onscreen before monitor sharing.
- Browser fixture runs a real autoplaying `<video>` backed by a 30 fps `MediaStream`, with 660 Hz process audio. Chromium native-occlusion backgrounding is disabled so a fully covered fixture cannot silently turn black or freeze.
- Four scenes switch to distinct magenta, cyan, yellow, and blue render markers. Geometry checks distinguish full-screen, cropped, and rotated picture-in-picture layouts.
- Program and Preview deliver sustained D3D11-composited motion. A single changed frame is insufficient.
- Live scene commands hide/show the source, reorder text across texture layers, move it into picture-in-picture, and restore the original layout without restarting the renderer.
- Two complete 1280×720/30 to 1920×1080/60 to 1280×720/30 cycles must emit the exact recovery events and keep rendering. Internal render cadence proves the 60 fps preset scales; external WGC cadence separately proves DWM capture health.
- Twenty-four rapid scene/audio transitions must recover to the requested final state.
- WASAPI process loopback captures browser audio and an independent 440 Hz process. Every measured stage records sample count, RMS, peak, DC offset, clipped-sample ratio, crest factor, and spectral amplitudes.
- Per-source volume and mute isolate both tones, verify actual 50% gain, contain both tones in the mix, impose an absolute mute ceiling, and recover after mute.
- Media-audio stress covers isolation, 25% gain, two-source limiter overload, pause/resume, and stereo 770 Hz left plus 1230 Hz right with bounded crosstalk.
- Fatal engine errors, audio warnings, unsupported media, or failed output recovery fail the report even if earlier measurements passed.

The MiroTalk receiver probe analyzes the remote browser `MediaStream` directly. The Discord receiver probe watches the visible client and captures its process audio. Both recognize the same markers, sustained motion, and 660 Hz/440 Hz mix after transport.

### Receiver acceptance gates

The direct MiroTalk oracle is deliberately stricter than a screenshot check:

| Area | Required proof |
| --- | --- |
| Run identity | Schema 2; receiver and publisher use the same validated run ID; wrapper accepts only a fresh report created after task start. |
| Tracks | Exactly one audio and one video track at both start and end; both enabled/live; zero `ended`, `mute`, or `unmute` events. Duplicate stale WebRTC tracks fail. |
| Video | At least 640×360; four connected marker panels recognized. Solid-color frames, sparse colored noise, and implausibly small/large components are rejected. |
| Sequence/continuity | Real `requestVideoFrameCallback` is mandatory. Marker transitions must follow browser, tone, mixed, muted cyclically from any starting phase. Every stage has at least 8 observed frames and an inter-frame gap below 2500 ms. Browser/tone/mixed each need at least 5 compared pairs, at least 40% moving pairs, median motion above `0.0002`, and no frozen run longer than 4 pairs. |
| Audio continuity | Four seconds, normally 192,000 samples at 48 kHz, retained for every stage; maximum Web Audio callback gap below 1500 ms. |
| Isolation | Browser 660 Hz and tone 440 Hz each exceed `0.002` and dominate the other bin by at least 3×. Mix contains both bins above `0.001` with a ratio from 0.1 to 10. |
| Silence/quality | Muted RMS below `0.01` and below 25% of active RMS; absolute DC offset below `0.02`, peak at most 1.0, clipped samples below 0.1%, and finite nonnegative crest factor in every stage. |

Run deterministic negative and harness-contract coverage after every harness change:

```bash
npm run test:windows-qualification
```

## Requirements

- Windows 11 in an interactive logged-in desktop session.
- Hardware D3D11 device and working default playback endpoint.
- Microsoft Edge or Google Chrome.
- For local transport: Node.js and official MiroTalk BRO pinned to the reviewed qualification commit.
- For Discord transport: Discord desktop, signed in and joined to a voice channel.
- For full transport proof, a second Discord account in the same voice channel. It may run on a second Windows desktop, or in regular Microsoft Edge on the same qualification VM while Discord desktop publishes.
- Rust toolchain matching this repository.

Do not run the live qualification as a service or ordinary hosted CI job. Windows Graphics Capture, Discord, and WASAPI need the interactive user desktop and audio endpoint. Existing Windows CI still compiles the probes and runs deterministic Windows unit tests.

## 1. Native publisher qualification

No Discord required:

```powershell
pwsh -File .\scripts\windows-discord\Start-Publisher.ps1 -NativeOnly
```

Result: `target\windows-discord\publisher-native.json`. Exit code is non-zero if any scene, frame-motion, output-resize, audio-isolation, mix, mute, or limiter assertion fails.

## 2. Local MiroTalk BRO E2E

MiroTalk BRO is used from its [official repository](https://github.com/miroslavpejic85/mirotalkbro), pinned to commit `d932edddf9cf04ac96a305af753ef27a021630db` (`1.3.77`, AGPLv3). Prepare the isolated checkout through the qualification skill helper, then start it only on loopback:

```bash
skills/hooviestar-windows-discord-qualification/scripts/prepare-mirotalk-bro.sh
skills/hooviestar-windows-discord-qualification/scripts/start-mirotalk-bro.sh
```

Require `ss` readback for `127.0.0.1:3016` only. Refuse the run if any LAN, VPN, container, or tailnet address can reach port 3016. MiroTalk's `HOST` setting changes generated URLs but does not reliably constrain `server.listen`; the skill's Node preload enforces the bind.

Forward guest `127.0.0.1:3016` back to that server. Forward guest CDP ports `9223` and `9224` to host loopback ports `19223` and `19224`. Start two interactive Edge app windows in the VM:

```powershell
.\scripts\windows-discord\Start-MiroTalkClient.ps1 -Role Publisher -CdpPort 9223
.\scripts\windows-discord\Start-MiroTalkClient.ps1 -Role Receiver -CdpPort 9224
```

Start both clients from clean isolated profiles. The launcher kills only the prior qualification profile's process tree, waits for the CDP port to become free, and proves `http://127.0.0.1:<port>/json/version` before reporting success. Create `target\windows-discord\share-picker-open.gate`, then run the publisher task with at least a 300-second hold and a unique run ID. Native assertions and the same-run `system-audio-fixtures\preflight.json` must pass before opening the share picker. While the gate exists, Program releases topmost state so the trusted Edge picker stays visible.

Trigger the picker through publisher CDP:

```bash
node scripts/windows-discord/Control-MiroTalkCdp.mjs publisher 19223
```

Bring the picker forward by clicking the publisher's taskbar icon if Program still covers it. Select `Entire Screen`, select the display, enable system audio, and click `Share with Audio`. Coordinate-based QMP input is allowed only after a fresh screenshot before every action confirms the 1280×800 picker layout and enabled control state. Send pointer movement, button-down, and button-up as separate QMP calls, with at least 80 ms between down and up; one combined event batch moves the pointer but can lose the click on this VM. Publisher status must show exactly one live `System Audio` track at 48 kHz, exactly one live `screen:0:0` monitor track, connected ICE/peer state, stable signaling, and no captured console errors.

After the share becomes live, restart the receiver qualification profile once and rejoin the room. This removes any transceivers created by a receiver that joined before publication. Do not reuse a publisher page after an aborted or repeated picker attempt: MiroTalk can retain ended sender tracks and deliver duplicate live receiver audio tracks. Restart both profiles and create a new run instead. Remove the gate only after the clean publisher and receiver each show exactly one live audio and video track; Program becomes topmost again at the next stage.

Prevent local feedback without destroying the received audio track, then run the direct-stream oracle:

```bash
node scripts/windows-discord/Control-MiroTalkCdp.mjs receiver-muted 19224
node scripts/windows-discord/Qualify-MiroTalkReceiver.mjs \
  --port 19224 \
  --duration 112 \
  --report target/windows-discord/mirotalk-receiver.json \
  --run-id <publisher-run-id>
```

Do not start Edge with global `--mute-audio`; that makes Web Audio return zeros. The controller mutes only `#mainVideo`, while the oracle analyzes its live remote audio track. Pass requires four markers, motion in three video stages, isolated 660 Hz and 440 Hz stages, both in the mix, quiet mute, live tracks, and nonzero dimensions.

Full MiroTalk evidence requires:

- publisher scheduled-task result `0` and task status `exitCode: 0` after clean teardown;
- `publisher-mirotalk-native.json` with `passed: true`, fresh timestamp, and expected run ID;
- `system-audio-fixtures\preflight.json` with `passed: true`, onscreen Program, and the same run ID;
- publisher and receiver CDP status with exact live track counts;
- `mirotalk-receiver.json` with `passed: true` and the same run ID;
- pinned MiroTalk commit/license plus loopback-only listener readback;
- no remaining `source-session-restore.json` after clean publisher teardown.

## 3. Real Discord publisher

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

## 4. Discord receiver measurement

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
- Low sustained-motion fraction or long frozen run: browser video stopped, Chromium occlusion throttled a covered fixture, Windows capture stalled, or transport delivered repeated frames. Do not lower the threshold until native and display-frame comparisons separate these causes.
- Missing 660 Hz: browser autoplay/audio session unavailable, browser audio source failed, or Discord share sound disabled.
- Missing 440 Hz: independent application-audio capture or mix failed.
- High cross-frequency leakage in a single-source stage: mute/volume command did not reach mixer or stale audio was transmitted.
- High muted RMS: unrelated Discord audio, source-session leak, or mixer mute failure.
- Publisher passes but receiver fails: local Hooviestar path works; failure lies in Discord selection, share-sound configuration, transport, or receiver playback/capture.
- Duplicate or ended remote tracks: an aborted/repeated share reused stale MiroTalk transceivers. Restart clean publisher and receiver profiles; do not select one convenient track and ignore the others.
- CDP answers only on `::1`: previous browser tree or IPv4 listener did not release cleanly. Launcher must fail/retry; do not silently change evidence ports.

The full report chain is required evidence. A green CI run alone is not equivalent.

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
