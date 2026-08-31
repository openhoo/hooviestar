---
name: hooviestar-windows-discord-qualification
description: Provision, run, diagnose, and collect evidence for Hooviestar scene, browser-video, output-resize, Windows application-audio mixing, local MiroTalk BRO WebRTC transport, and real Discord screen-share qualification in an interactive Windows 11 VM.
---

# Hooviestar Windows Discord Qualification

## Scope

Use for Windows VM, Discord/MiroTalk screen share, Program/Preview, scenes, browser video, application audio, mute/volume, mixing, resize, cadence, and receiver transport qualification.

Read `../../docs/windows-discord-qualification.md` and each invoked script under `../../scripts/windows-discord/` before acting. Preserve dirty workspace changes. Stage the exact tree without `.git`, `target`, `node_modules`, or `dist`; never branch-switch, stash, reset, or overwrite user work.

## Evidence contract

Never infer success from compilation, CI, a screenshot, or one report.

- Native proof: interactive task result `0`; fresh same-run `publisher-native.json` with `passed: true`; clean teardown.
- MiroTalk proof: native report and system-audio preflight passed; publisher has exactly one live 48 kHz `System Audio` and one live monitor-video track; clean receiver has exactly one live remote audio/video track at start and end; direct-stream receiver report passed; publisher task result `0`; no restore journal.
- Discord proof: publisher native report plus a fresh same-run `discord-receiver.json` with `passed: true`.
- Preserve task stdout/stderr/status, reports, picker screenshots, exact transport version/pin, listener readback, and failure-before-fix evidence under the host evidence root.
- Report native, MiroTalk, and real Discord separately. Authentication/manual picker boundaries stay explicit.

## Safety

- Use only the `hooviestar-win11-discord-qual` copy-on-write VM. Never mutate the base qcow2.
- Keep SSH, SPICE, MiroTalk, and CDP listeners on loopback.
- Never print VM credentials or Discord credentials/tokens. Discord sign-in, MFA, server, channel, and consent remain manual.
- Run graphics/audio qualification in an interactive logged-in desktop with active SPICE audio, never as a service or ordinary hosted CI job.
- Use only official MiroTalk BRO commit `d932edddf9cf04ac96a305af753ef27a021630db`, package `1.3.77`, AGPLv3. Require clean checkout and loopback listener readback.
- Keep VM and evidence unless deletion is explicitly requested.

## Workflow

1. Inspect qcow2 chain, domain, local port bindings, interactive `explorer.exe`, display power state, and SPICE audio. Capture screenshots before any coordinate input.
2. Stage exact workspace. Run host `npm run test:windows-qualification`. In Windows run `cargo test --workspace`, then build qualification examples with `cargo build --release`.
3. Run native publisher through an interactive-token scheduled task. Require scene geometry; connected markers; sustained Program/Preview motion; live hide/show/reorder/transform/restore; two resize/recovery cycles; separate internal-render and WGC cadence; rapid transitions; isolated/gained/mixed/muted audio; limiter, pause/resume and stereo stress; clean event stream and teardown.
4. Prepare and start MiroTalk with the helpers in `scripts/`. Require only `127.0.0.1:3016`; refuse LAN/VPN/tailnet/container reachability. Reverse-forward guest 3016 and locally forward guest CDP 9223/9224.
5. Start clean isolated publisher/receiver profiles through interactive tasks. Launcher must stop the old profile tree, wait for port release, and prove IPv4 CDP `/json/version`. Chromium occlusion backgrounding must be disabled.
6. Create `share-picker-open.gate`; start publisher with at least 300 seconds and a unique run ID. Wait for fresh same-run native and system-audio preflight passes.
7. Trigger publisher picker through `Control-MiroTalkCdp.mjs`. Before every QMP coordinate, capture a fresh 1280x800 screenshot. Select Entire Screen, display, system audio, and Share with Audio using separate move/down/up events and at least 80 ms press time.
8. Require exact clean publisher tracks and stable peer state. Restart receiver after publication; require exact clean receiver tracks. After any aborted/repeated share, restart both profiles and use a new run ID.
9. Remove gate. Run `Qualify-MiroTalkReceiver.mjs` for 112 seconds with the publisher run ID. Require real `requestVideoFrameCallback`, cyclic marker order, connected marker components, bounded callback gaps, sustained motion, four seconds of audio per stage, spectral isolation/mix, DC/clipping/crest ceilings, absolute mute, exact tracks, and zero ended/mute/unmute events.
10. Wait for publisher task result `0`; verify task status, freshness/run identity, no restore journal, pinned server, listener isolation, and collected evidence.
11. For real Discord, use signed Discord desktop as publisher and a second account as receiver. Share `Hooviestar - Program` as an application with sound at 720p/30 or better. Run the Discord receiver wrapper and require its measured report. Do not treat MiroTalk as proof that Discord passed.

## Failure rules

- Native passes but remote motion freezes: compare visible VM frames. If the covered fixture becomes black/static, keep `--disable-backgrounding-occluded-windows` and `--disable-features=CalculateNativeWinOcclusion`; never weaken the oracle.
- Duplicate/ended MiroTalk tracks: stale transceivers from a reused page. Restart both profiles and new run ID.
- CDP on `::1` only: old process tree/IPv4 port did not release. Fail launch; do not silently change the evidence route.
- All-zero receiver audio: never use global `--mute-audio`; mute only the receiver video element and analyze its remote track.
- Low-level engine/audio/recovery event, missing marker, cadence failure, clipping/DC breach, noisy mute, callback gap, stale report, or teardown failure means failed qualification.

## Handoff

Return exact VM/Windows/Rust/transport versions, task results, run IDs, decisive native/receiver metrics, confirmed defects and fixes, evidence path, pushed commit, and any untested real-Discord boundary.
