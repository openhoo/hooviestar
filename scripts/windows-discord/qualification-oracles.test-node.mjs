import assert from 'node:assert/strict';
import test from 'node:test';

import {
    analyzeSignal,
    browserOracleSource,
    classifyMarkerPixels,
    evaluateReceiverReport,
    summarizeMotion,
} from './qualification-oracles.mjs';

test('browser injection source is self-contained', () => {
    const injected = Function(`${browserOracleSource()}; return { classifyMarkerPixels, evaluateReceiverReport };`)();
    assert.equal(typeof injected.classifyMarkerPixels, 'function');
    assert.equal(typeof injected.evaluateReceiverReport, 'function');
});

function rgbaFrame(width, height, color) {
    const pixels = new Uint8ClampedArray(width * height * 4);
    for (let offset = 0; offset < pixels.length; offset += 4) pixels.set(color, offset);
    return pixels;
}

function drawMarker(pixels, width, color) {
    for (let y = 18; y < 66; y++) {
        for (let x = 12; x < 192; x++) pixels.set(color, (y * width + x) * 4);
    }
}

test('connected marker panel accepted while solid screen and sparse noise rejected', () => {
    const width = 640;
    const height = 360;
    const valid = rgbaFrame(width, height, [16, 20, 24, 255]);
    drawMarker(valid, width, [255, 0, 255, 255]);
    assert.equal(classifyMarkerPixels(valid, width, height).marker, 'browser');
    assert.equal(classifyMarkerPixels(rgbaFrame(width, height, [255, 0, 255, 255]), width, height), undefined);
    const noise = rgbaFrame(width, height, [16, 20, 24, 255]);
    for (let offset = 0; offset < noise.length; offset += 4 * 97) noise.set([255, 0, 255, 255], offset);
    assert.equal(classifyMarkerPixels(noise, width, height), undefined);
});

test('single motion spike cannot pass sustained-motion requirements', () => {
    const spike = summarizeMotion([0, 0, 0.2, 0, 0, 0]);
    assert.equal(spike.maximumMotionRatio, 0.2);
    assert.ok(spike.movingFrameFraction < 0.4);
    assert.equal(spike.medianMotionRatio, 0);
    const live = summarizeMotion([0.01, 0.02, 0, 0.03, 0.04, 0.05]);
    assert.ok(live.movingFrameFraction >= 0.4);
    assert.ok(live.medianMotionRatio > 0.0002);
});

test('signal metrics expose sample length gain bins clipping and DC', () => {
    const samples = Array.from({ length: 48_000 }, (_, index) =>
        0.2 * Math.sin(Math.PI * 2 * 660 * index / 48_000) +
        0.005 * Math.sin(Math.PI * 2 * 440 * index / 48_000));
    const metrics = analyzeSignal(samples, 48_000);
    assert.equal(metrics.sampleCount, 48_000);
    assert.ok(metrics.amplitudes['660Hz'] > metrics.amplitudes['440Hz'] * 30);
    assert.ok(Math.abs(metrics.dcOffset) < 1e-12);
    assert.equal(metrics.clippedSampleRatio, 0);
});

function passingReport() {
    const stage = (frequency) => ({
        observedFrames: 40,
        maximumInterFrameGapMs: 100,
        motion: summarizeMotion([0.01, 0.02, 0.03, 0.02, 0.01, 0.04]),
        signal: {
            sampleCount: 192_000,
            rms: 0.1,
            peak: 0.2,
            dcOffset: 0,
            clippedSampleRatio: 0,
            crestFactor: 2,
            amplitudes: { '660Hz': frequency === 660 ? 0.1 : 0.0001, '440Hz': frequency === 440 ? 0.1 : 0.0001 },
        },
    });
    const report = {
        receiverMuted: true,
        receivedVideoWidth: 1152,
        receivedVideoHeight: 720,
        videoObservationMode: 'requestVideoFrameCallback',
        markerTransitions: [
            { marker: 'tone' },
            { marker: 'mixed' },
            { marker: 'muted' },
            { marker: 'browser' },
        ],
        targetStageSamples: 192_000,
        tracksAtStart: [{ kind: 'audio' }, { kind: 'video' }],
        tracksAtEnd: [{ kind: 'audio' }, { kind: 'video' }],
        trackCountsAtStart: { audio: 1, video: 1 },
        trackCountsAtEnd: { audio: 1, video: 1 },
        liveTrackCountsAtStart: { audio: 1, video: 1 },
        liveTrackCountsAtEnd: { audio: 1, video: 1 },
        videoTrackAtEnd: { readyState: 'live', enabled: true },
        audioTrackAtEnd: { readyState: 'live', enabled: true },
        trackEvents: { ended: 0, mute: 0, unmute: 0 },
        audioCallback: { count: 100, maximumGapMs: 100 },
        browser: stage(660),
        tone: stage(440),
        mixed: stage(),
        muted: stage(),
    };
    report.mixed.signal.amplitudes = { '660Hz': 0.05, '440Hz': 0.04 };
    report.muted.signal.rms = 0.0001;
    report.muted.signal.amplitudes = { '660Hz': 0, '440Hz': 0 };
    return report;
}

test('receiver gate rejects freeze spike stale track bad mix and noisy mute', () => {
    assert.equal(evaluateReceiverReport(passingReport()), true);
    const frozen = passingReport();
    frozen.browser.motion = summarizeMotion([0, 0, 0.3, 0, 0, 0]);
    assert.equal(evaluateReceiverReport(frozen), false);
    const ended = passingReport();
    ended.audioTrackAtEnd.readyState = 'ended';
    assert.equal(evaluateReceiverReport(ended), false);
    const imbalanced = passingReport();
    imbalanced.mixed.signal.amplitudes['660Hz'] = 0.00001;
    assert.equal(evaluateReceiverReport(imbalanced), false);
    const noisyMute = passingReport();
    noisyMute.muted.signal.rms = 0.02;
    assert.equal(evaluateReceiverReport(noisyMute), false);
    const duplicateAudio = passingReport();
    duplicateAudio.tracksAtStart.push({ kind: 'audio' });
    duplicateAudio.trackCountsAtStart.audio = 2;
    duplicateAudio.liveTrackCountsAtStart.audio = 2;
    assert.equal(evaluateReceiverReport(duplicateAudio), false);
    const wrongOrder = passingReport();
    wrongOrder.markerTransitions[2].marker = 'browser';
    assert.equal(evaluateReceiverReport(wrongOrder), false);
    const timerFallback = passingReport();
    timerFallback.videoObservationMode = 'timer-fallback';
    assert.equal(evaluateReceiverReport(timerFallback), false);
    const trackMute = passingReport();
    trackMute.trackEvents.mute = 1;
    assert.equal(evaluateReceiverReport(trackMute), false);
    const clipping = passingReport();
    clipping.mixed.signal.clippedSampleRatio = 0.01;
    assert.equal(evaluateReceiverReport(clipping), false);
});
