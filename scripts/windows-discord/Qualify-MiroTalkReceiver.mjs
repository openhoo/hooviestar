#!/usr/bin/env node

import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';

import { browserOracleSource } from './qualification-oracles.mjs';

const arguments_ = process.argv.slice(2);
const value = (name, fallback) => {
    const index = arguments_.indexOf(name);
    return index >= 0 ? arguments_[index + 1] : fallback;
};
const port = Number(value('--port', '19224'));
const durationSeconds = Number(value('--duration', '112'));
const reportPath = resolve(value('--report', 'target/windows-discord/mirotalk-receiver.json'));
const qualificationRunId = value('--run-id', 'unlinked');
if (!Number.isInteger(port) || port < 1 || port > 65535 || !Number.isFinite(durationSeconds) || durationSeconds < 32) {
    console.error('usage: Qualify-MiroTalkReceiver.mjs [--port PORT] [--duration SECONDS] [--report PATH] [--run-id ID]');
    process.exit(2);
}
if (!/^[A-Za-z0-9._-]+$/.test(qualificationRunId)) throw new Error('Invalid --run-id');

const targetsResponse = await fetch(`http://127.0.0.1:${port}/json/list`, {
    signal: AbortSignal.timeout(10_000),
});
if (!targetsResponse.ok) throw new Error(`CDP target list returned HTTP ${targetsResponse.status}`);
const targets = await targetsResponse.json();
const target = targets.find(
    (entry) => entry.type === 'page' && entry.url.includes('127.0.0.1:3016/views/viewer.html'),
);
if (!target) throw new Error(`No MiroTalk receiver target on CDP port ${port}`);

const socket = new WebSocket(target.webSocketDebuggerUrl);
let openTimeout;
try {
    await new Promise((resolveOpen, reject) => {
        openTimeout = setTimeout(() => reject(new Error('CDP WebSocket open timed out')), 10_000);
        socket.addEventListener('open', resolveOpen, { once: true });
        socket.addEventListener('error', reject, { once: true });
    });
} finally {
    clearTimeout(openTimeout);
}
let sequence = 0;
const pending = new Map();
socket.addEventListener('message', (event) => {
    const message = JSON.parse(event.data);
    if (!message.id || !pending.has(message.id)) return;
    const { resolve: resolvePending, reject, timeout } = pending.get(message.id);
    clearTimeout(timeout);
    pending.delete(message.id);
    if (message.error) reject(new Error(message.error.message));
    else resolvePending(message.result);
});
const send = (method, params = {}, timeoutMilliseconds = 15_000) => {
    const id = ++sequence;
    socket.send(JSON.stringify({ id, method, params }));
    return new Promise((resolvePending, reject) => {
        const timeout = setTimeout(() => {
            pending.delete(id);
            reject(new Error(`CDP command timed out: ${method}`));
        }, timeoutMilliseconds);
        pending.set(id, { resolve: resolvePending, reject, timeout });
    });
};

const frameTree = await send('Page.getFrameTree');
const findReceiverFrame = (tree) => {
    if (tree.frame.url.includes('/views/viewer.html')) return tree.frame;
    for (const child of tree.childFrames ?? []) {
        const match = findReceiverFrame(child);
        if (match) return match;
    }
    return undefined;
};
const receiverFrame = findReceiverFrame(frameTree.frameTree);
if (!receiverFrame) throw new Error('MiroTalk receiver frame has not committed');
const world = await send('Page.createIsolatedWorld', {
    frameId: receiverFrame.id,
    worldName: 'hooviestar-receiver-oracle',
    grantUniveralAccess: true,
});

const expression = `(async () => {
    ${browserOracleSource()}
    const schemaVersion = 2;
    const qualificationRunId = ${JSON.stringify(qualificationRunId)};
    const measurementStarted = new Date().toISOString();
    const durationMilliseconds = ${JSON.stringify(durationSeconds * 1000)};
    const video = document.querySelector('#mainVideo');
    if (!video) throw new Error('MiroTalk mainVideo is missing');
    video.muted = true;
    video.volume = 0;
    await video.play().catch(() => undefined);

    const readyDeadline = performance.now() + 30000;
    while (
        performance.now() < readyDeadline &&
        (!video.srcObject ||
            video.readyState < 2 ||
            video.srcObject.getAudioTracks().every((track) => track.readyState !== 'live') ||
            video.srcObject.getVideoTracks().every((track) => track.readyState !== 'live'))
    ) {
        await new Promise((resolveDelay) => setTimeout(resolveDelay, 100));
    }
    const stream = video.srcObject;
    if (!stream) throw new Error('MiroTalk receiver has no remote stream');
    const allTracksAtStart = stream.getTracks();
    const liveAudioTracks = stream.getAudioTracks().filter((track) => track.readyState === 'live');
    const liveVideoTracks = stream.getVideoTracks().filter((track) => track.readyState === 'live');
    const audioTrack = liveAudioTracks[0];
    const videoTrack = liveVideoTracks[0];
    if (!audioTrack || !videoTrack) throw new Error('MiroTalk receiver tracks are not both live');

    const trackDescription = (track) => ({
        kind: track.kind,
        label: track.label,
        enabled: track.enabled,
        muted: track.muted,
        readyState: track.readyState,
        settings: track.getSettings(),
    });
    const tracksAtStart = allTracksAtStart.map(trackDescription);
    const trackCountsAtStart = {
        audio: allTracksAtStart.filter((track) => track.kind === 'audio').length,
        video: allTracksAtStart.filter((track) => track.kind === 'video').length,
    };
    const liveTrackCountsAtStart = { audio: liveAudioTracks.length, video: liveVideoTracks.length };
    const trackEvents = { ended: 0, mute: 0, unmute: 0 };
    for (const track of allTracksAtStart) {
        track.addEventListener('ended', () => trackEvents.ended++);
        track.addEventListener('mute', () => trackEvents.mute++);
        track.addEventListener('unmute', () => trackEvents.unmute++);
    }

    const names = ['browser', 'tone', 'mixed', 'muted'];
    const frames = Object.fromEntries(names.map((name) => [name, 0]));
    const markerTransitions = [];
    const motionRatios = Object.fromEntries(names.map((name) => [name, []]));
    const maximumInterFrameGap = Object.fromEntries(names.map((name) => [name, 0]));
    const markerConfidence = Object.fromEntries(names.map((name) => [name, 1]));
    const samples = Object.fromEntries(names.map((name) => [name, []]));
    let previousPixels;
    let previousFrameTime;
    let currentMarker;
    let markerSince = performance.now();
    let currentSamples = [];

    const finalizeVisit = () => {
        if (currentMarker && currentSamples.length > samples[currentMarker].length) {
            samples[currentMarker] = currentSamples;
        }
        currentSamples = [];
    };
    const enterMarker = (marker, now) => {
        if (currentMarker === marker) return;
        finalizeVisit();
        currentMarker = marker;
        markerTransitions.push({ marker, elapsedMs: now });
        markerSince = now;
        previousPixels = undefined;
        previousFrameTime = undefined;
    };

    const canvas = document.createElement('canvas');
    canvas.width = 288;
    canvas.height = 180;
    const context = canvas.getContext('2d', { willReadFrequently: true });
    const observeVideo = (now) => {
        if (video.videoWidth < 1 || video.videoHeight < 1) return;
        context.drawImage(video, 0, 0, canvas.width, canvas.height);
        const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
        const observation = classifyMarkerPixels(pixels, canvas.width, canvas.height);
        if (!observation) return;
        enterMarker(observation.marker, now);
        frames[currentMarker]++;
        markerConfidence[currentMarker] = Math.min(
            markerConfidence[currentMarker],
            observation.componentFraction,
        );
        if (previousFrameTime !== undefined) {
            maximumInterFrameGap[currentMarker] = Math.max(
                maximumInterFrameGap[currentMarker],
                now - previousFrameTime,
            );
        }
        if (previousPixels) {
            let changed = 0;
            let compared = 0;
            for (let offset = 0; offset < pixels.length; offset += 32) {
                compared++;
                const delta =
                    Math.abs(previousPixels[offset] - pixels[offset]) +
                    Math.abs(previousPixels[offset + 1] - pixels[offset + 1]) +
                    Math.abs(previousPixels[offset + 2] - pixels[offset + 2]);
                if (delta > 36) changed++;
            }
            motionRatios[currentMarker].push(compared ? changed / compared : 0);
        }
        previousPixels = new Uint8ClampedArray(pixels);
        previousFrameTime = now;
    };

    let stopVideoObservation = false;
    let frameCallbackId;
    let fallbackInterval;
    const videoObservationMode = typeof video.requestVideoFrameCallback === 'function'
        ? 'requestVideoFrameCallback'
        : 'timer-fallback';
    if (videoObservationMode === 'requestVideoFrameCallback') {
        const onFrame = (now) => {
            if (stopVideoObservation) return;
            observeVideo(now);
            frameCallbackId = video.requestVideoFrameCallback(onFrame);
        };
        frameCallbackId = video.requestVideoFrameCallback(onFrame);
    } else {
        fallbackInterval = setInterval(() => observeVideo(performance.now()), 100);
    }

    const audioContext = new AudioContext();
    await audioContext.resume();
    const sampleRate = audioContext.sampleRate;
    const targetStageSamples = Math.round(sampleRate * 4);
    const source = audioContext.createMediaStreamSource(new MediaStream([audioTrack]));
    const processor = audioContext.createScriptProcessor(2048, 2, 1);
    const silent = audioContext.createGain();
    silent.gain.value = 0;
    let audioCallbacks = 0;
    let previousAudioCallback;
    let maximumAudioCallbackGap = 0;
    processor.onaudioprocess = (event) => {
        const now = performance.now();
        audioCallbacks++;
        if (previousAudioCallback !== undefined) {
            maximumAudioCallbackGap = Math.max(maximumAudioCallbackGap, now - previousAudioCallback);
        }
        previousAudioCallback = now;
        if (!currentMarker || now - markerSince < 1000 || currentSamples.length >= targetStageSamples) return;
        const input = event.inputBuffer;
        const left = input.getChannelData(0);
        const right = input.numberOfChannels > 1 ? input.getChannelData(1) : left;
        const remaining = targetStageSamples - currentSamples.length;
        for (let index = 0; index < Math.min(left.length, remaining); index++) {
            currentSamples.push((left[index] + right[index]) * 0.5);
        }
    };
    source.connect(processor);
    processor.connect(silent);
    silent.connect(audioContext.destination);

    const started = performance.now();
    while (performance.now() - started < durationMilliseconds) {
        await new Promise((resolveDelay) => setTimeout(resolveDelay, 200));
    }
    finalizeVisit();
    stopVideoObservation = true;
    if (frameCallbackId !== undefined && typeof video.cancelVideoFrameCallback === 'function') {
        video.cancelVideoFrameCallback(frameCallbackId);
    }
    if (fallbackInterval !== undefined) clearInterval(fallbackInterval);
    processor.disconnect();
    source.disconnect();
    silent.disconnect();
    await audioContext.close();

    const allTracksAtEnd = stream.getTracks();
    const tracksAtEnd = allTracksAtEnd.map(trackDescription);
    const trackCountsAtEnd = {
        audio: allTracksAtEnd.filter((track) => track.kind === 'audio').length,
        video: allTracksAtEnd.filter((track) => track.kind === 'video').length,
    };
    const liveTrackCountsAtEnd = {
        audio: allTracksAtEnd.filter((track) => track.kind === 'audio' && track.readyState === 'live').length,
        video: allTracksAtEnd.filter((track) => track.kind === 'video' && track.readyState === 'live').length,
    };

    const stages = Object.fromEntries(names.map((name) => {
        const motion = summarizeMotion(motionRatios[name]);
        return [name, {
            marker: name,
            observedFrames: frames[name],
            markerComponentFraction: markerConfidence[name] === 1 ? 0 : markerConfidence[name],
            maximumMotionRatio: motion.maximumMotionRatio,
            maximumInterFrameGapMs: maximumInterFrameGap[name] || Number.MAX_SAFE_INTEGER,
            motion,
            signal: analyzeSignal(samples[name], sampleRate),
        }];
    }));
    const report = {
        schemaVersion,
        qualificationRunId,
        passed: false,
        transport: 'MiroTalk BRO',
        measurementStarted,
        measurementCompleted: new Date().toISOString(),
        receiverMuted: video.muted && video.volume === 0,
        sampleRate,
        targetStageSamples,
        receivedVideoWidth: video.videoWidth,
        receivedVideoHeight: video.videoHeight,
        videoObservationMode,
        markerTransitions,
        tracks: tracksAtStart,
        tracksAtStart,
        tracksAtEnd,
        trackCountsAtStart,
        trackCountsAtEnd,
        liveTrackCountsAtStart,
        liveTrackCountsAtEnd,
        audioTrackAtEnd: trackDescription(audioTrack),
        videoTrackAtEnd: trackDescription(videoTrack),
        trackEvents,
        audioCallback: { count: audioCallbacks, maximumGapMs: maximumAudioCallbackGap },
        ...stages,
    };
    report.passed = evaluateReceiverReport(report);
    return report;
})()`;

let result;
try {
    result = await send('Runtime.evaluate', {
        expression,
        contextId: world.executionContextId,
        awaitPromise: true,
        returnByValue: true,
        userGesture: true,
        timeout: durationSeconds * 1000 + 60_000,
    }, durationSeconds * 1000 + 65_000);
} finally {
    socket.close();
}
if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
}
const report = result.result.value;
await mkdir(dirname(reportPath), { recursive: true });
await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
console.log(JSON.stringify(report, null, 2));
if (!report.passed) process.exit(1);
