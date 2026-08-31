#!/usr/bin/env node

import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';

const arguments_ = process.argv.slice(2);
const value = (name, fallback) => {
    const index = arguments_.indexOf(name);
    return index >= 0 ? arguments_[index + 1] : fallback;
};
const port = Number(value('--port', '19224'));
const durationSeconds = Number(value('--duration', '112'));
const reportPath = resolve(value('--report', 'target/windows-discord/mirotalk-receiver.json'));
if (!Number.isInteger(port) || port < 1 || port > 65535 || !Number.isFinite(durationSeconds) || durationSeconds < 32) {
    console.error('usage: Qualify-MiroTalkReceiver.mjs [--port PORT] [--duration SECONDS] [--report PATH]');
    process.exit(2);
}

const targets = await fetch(`http://127.0.0.1:${port}/json/list`).then((response) => response.json());
const target = targets.find(
    (entry) => entry.type === 'page' && entry.url.includes('127.0.0.1:3016/views/viewer.html'),
);
if (!target) throw new Error(`No MiroTalk receiver target on CDP port ${port}`);

const socket = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
    socket.addEventListener('open', resolve, { once: true });
    socket.addEventListener('error', reject, { once: true });
});
let sequence = 0;
const pending = new Map();
socket.addEventListener('message', (event) => {
    const message = JSON.parse(event.data);
    if (!message.id || !pending.has(message.id)) return;
    const { resolve: resolvePending, reject } = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) reject(new Error(message.error.message));
    else resolvePending(message.result);
});
const send = (method, params = {}) => {
    const id = ++sequence;
    socket.send(JSON.stringify({ id, method, params }));
    return new Promise((resolvePending, reject) => pending.set(id, { resolve: resolvePending, reject }));
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
        await new Promise((resolve) => setTimeout(resolve, 100));
    }
    const stream = video.srcObject;
    if (!stream) throw new Error('MiroTalk receiver has no remote stream');
    const audioTrack = stream.getAudioTracks().find((track) => track.readyState === 'live');
    const videoTrack = stream.getVideoTracks().find((track) => track.readyState === 'live');
    if (!audioTrack || !videoTrack) throw new Error('MiroTalk receiver tracks are not both live');

    const trackDescription = (track) => ({
        kind: track.kind,
        label: track.label,
        enabled: track.enabled,
        muted: track.muted,
        readyState: track.readyState,
        settings: track.getSettings(),
    });
    const names = ['browser', 'tone', 'mixed', 'muted'];
    const frames = Object.fromEntries(names.map((name) => [name, 0]));
    const motion = Object.fromEntries(names.map((name) => [name, 0]));
    const samples = Object.fromEntries(names.map((name) => [name, []]));
    const previous = {};
    let currentMarker;
    let markerSince = performance.now();

    const canvas = document.createElement('canvas');
    canvas.width = 288;
    canvas.height = 180;
    const context = canvas.getContext('2d', { willReadFrequently: true });
    const classify = (pixels) => {
        const counts = [0, 0, 0, 0];
        for (let offset = 0; offset < pixels.length; offset += 16) {
            const red = pixels[offset];
            const green = pixels[offset + 1];
            const blue = pixels[offset + 2];
            if (red > 180 && blue > 180 && green < 110) counts[0]++;
            else if (green > 180 && blue > 180 && red < 110) counts[1]++;
            else if (red > 180 && green > 180 && blue < 110) counts[2]++;
            else if (blue > 180 && red < 100 && green < 100) counts[3]++;
        }
        let index = 0;
        for (let candidate = 1; candidate < counts.length; candidate++) {
            if (counts[candidate] > counts[index]) index = candidate;
        }
        return counts[index] >= pixels.length / 4 / 4 / 500 ? names[index] : undefined;
    };
    const observeVideo = () => {
        if (video.videoWidth < 1 || video.videoHeight < 1) return;
        context.drawImage(video, 0, 0, canvas.width, canvas.height);
        const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
        const marker = classify(pixels);
        if (!marker) return;
        if (currentMarker !== marker) {
            currentMarker = marker;
            markerSince = performance.now();
            // Keep stable samples gathered in prior cycles. Each eight-second
            // visit contributes only after its one-second transition guard;
            // resetting here can make a valid stage miss the four-second
            // quota when Web Audio callbacks are throttled by the VM.
        }
        frames[marker]++;
        const older = previous[marker];
        if (older) {
            let changed = 0;
            let compared = 0;
            for (let offset = 0; offset < pixels.length; offset += 32) {
                compared++;
                const delta =
                    Math.abs(older[offset] - pixels[offset]) +
                    Math.abs(older[offset + 1] - pixels[offset + 1]) +
                    Math.abs(older[offset + 2] - pixels[offset + 2]);
                if (delta > 36) changed++;
            }
            motion[marker] = Math.max(motion[marker], compared ? changed / compared : 0);
        }
        previous[marker] = new Uint8ClampedArray(pixels);
    };

    const audioContext = new AudioContext();
    await audioContext.resume();
    const sampleRate = audioContext.sampleRate;
    const targetStageSamples = Math.round(sampleRate * 4);
    const source = audioContext.createMediaStreamSource(new MediaStream([audioTrack]));
    const processor = audioContext.createScriptProcessor(2048, 2, 1);
    const silent = audioContext.createGain();
    silent.gain.value = 0;
    processor.onaudioprocess = (event) => {
        if (!currentMarker || performance.now() - markerSince < 1000) return;
        const bucket = samples[currentMarker];
        if (bucket.length >= targetStageSamples) return;
        const input = event.inputBuffer;
        const left = input.getChannelData(0);
        const right = input.numberOfChannels > 1 ? input.getChannelData(1) : left;
        const remaining = targetStageSamples - bucket.length;
        for (let index = 0; index < Math.min(left.length, remaining); index++) {
            bucket.push((left[index] + right[index]) * 0.5);
        }
    };
    source.connect(processor);
    processor.connect(silent);
    silent.connect(audioContext.destination);

    const interval = setInterval(observeVideo, 200);
    const started = performance.now();
    while (performance.now() - started < durationMilliseconds) {
        await new Promise((resolve) => setTimeout(resolve, 200));
    }
    clearInterval(interval);
    processor.disconnect();
    source.disconnect();
    silent.disconnect();
    await audioContext.close();

    const analyze = (values) => {
        const count = Math.max(values.length, 1);
        let squares = 0;
        let peak = 0;
        const amplitudes = {};
        for (const sample of values) {
            squares += sample * sample;
            peak = Math.max(peak, Math.abs(sample));
        }
        for (const frequency of [660, 440]) {
            let real = 0;
            let imaginary = 0;
            for (let index = 0; index < values.length; index++) {
                const phase = Math.PI * 2 * frequency * index / sampleRate;
                const window = values.length <= 1
                    ? 1
                    : 0.5 - 0.5 * Math.cos(Math.PI * 2 * index / (values.length - 1));
                real += values[index] * window * Math.cos(phase);
                imaginary -= values[index] * window * Math.sin(phase);
            }
            amplitudes[frequency + 'Hz'] = 4 * Math.hypot(real, imaginary) / count;
        }
        return { sampleCount: values.length, rms: Math.sqrt(squares / count), peak, amplitudes };
    };
    const stages = Object.fromEntries(names.map((name) => [name, {
        marker: name,
        observedFrames: frames[name],
        maximumMotionRatio: motion[name],
        signal: analyze(samples[name]),
    }]));
    const activeRms = Math.max(stages.browser.signal.rms, stages.tone.signal.rms, stages.mixed.signal.rms);
    const passed =
        names.every((name) => stages[name].observedFrames >= 3 && stages[name].signal.sampleCount >= targetStageSamples) &&
        stages.browser.maximumMotionRatio > 0.0002 &&
        stages.tone.maximumMotionRatio > 0.0002 &&
        stages.mixed.maximumMotionRatio > 0.0002 &&
        stages.browser.signal.amplitudes['660Hz'] > 0.002 &&
        stages.browser.signal.amplitudes['660Hz'] > stages.browser.signal.amplitudes['440Hz'] * 3 &&
        stages.tone.signal.amplitudes['440Hz'] > 0.002 &&
        stages.tone.signal.amplitudes['440Hz'] > stages.tone.signal.amplitudes['660Hz'] * 3 &&
        stages.mixed.signal.amplitudes['660Hz'] > 0.001 &&
        stages.mixed.signal.amplitudes['440Hz'] > 0.001 &&
        stages.muted.signal.rms < activeRms * 0.25;
    return {
        passed,
        transport: 'MiroTalk BRO',
        receiverMuted: video.muted && video.volume === 0,
        sampleRate,
        receivedVideoWidth: video.videoWidth,
        receivedVideoHeight: video.videoHeight,
        tracks: [trackDescription(audioTrack), trackDescription(videoTrack)],
        ...stages,
    };
})()`;

const result = await send('Runtime.evaluate', {
    expression,
    contextId: world.executionContextId,
    awaitPromise: true,
    returnByValue: true,
    userGesture: true,
});
socket.close();
if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
}
const report = result.result.value;
await mkdir(dirname(reportPath), { recursive: true });
await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
console.log(JSON.stringify(report, null, 2));
if (!report.passed) process.exit(1);
