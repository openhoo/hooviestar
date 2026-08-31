export function markerIndex(red, green, blue) {
    if (red > 180 && blue > 180 && green < 110) return 0;
    if (green > 180 && blue > 180 && red < 110) return 1;
    if (red > 180 && green > 180 && blue < 110) return 2;
    if (blue > 180 && red < 100 && green < 100) return 3;
    return -1;
}

export function markerName(index) {
    return ['browser', 'tone', 'mixed', 'muted'][index];
}

export function classifyMarkerPixels(pixels, width, height) {
    const step = 4;
    const gridWidth = Math.ceil(width / step);
    const gridHeight = Math.ceil(height / step);
    const sampledPixels = gridWidth * gridHeight;
    if (!sampledPixels || pixels.length < width * height * 4) return undefined;
    const labels = new Int8Array(sampledPixels);
    labels.fill(-1);
    for (let gridY = 0; gridY < gridHeight; gridY++) {
        const y = Math.min(gridY * step, height - 1);
        for (let gridX = 0; gridX < gridWidth; gridX++) {
            const x = Math.min(gridX * step, width - 1);
            const offset = (y * width + x) * 4;
            labels[gridY * gridWidth + gridX] = markerIndex(
                pixels[offset],
                pixels[offset + 1],
                pixels[offset + 2],
            );
        }
    }

    const visited = new Uint8Array(sampledPixels);
    let best;
    for (let start = 0; start < sampledPixels; start++) {
        const label = labels[start];
        if (label < 0 || visited[start]) continue;
        const stack = [start];
        visited[start] = 1;
        let count = 0;
        let minX = gridWidth;
        let minY = gridHeight;
        let maxX = 0;
        let maxY = 0;
        while (stack.length) {
            const index = stack.pop();
            const x = index % gridWidth;
            const y = Math.floor(index / gridWidth);
            count++;
            minX = Math.min(minX, x);
            minY = Math.min(minY, y);
            maxX = Math.max(maxX, x);
            maxY = Math.max(maxY, y);
            const neighbors = [
                x > 0 ? index - 1 : -1,
                x + 1 < gridWidth ? index + 1 : -1,
                y > 0 ? index - gridWidth : -1,
                y + 1 < gridHeight ? index + gridWidth : -1,
            ];
            for (const neighbor of neighbors) {
                if (neighbor >= 0 && !visited[neighbor] && labels[neighbor] === label) {
                    visited[neighbor] = 1;
                    stack.push(neighbor);
                }
            }
        }
        const componentWidth = maxX - minX + 1;
        const componentHeight = maxY - minY + 1;
        const componentFraction = count / sampledPixels;
        const fillRatio = count / (componentWidth * componentHeight);
        const aspectRatio = componentWidth / componentHeight;
        const plausible =
            componentFraction >= 0.00125 &&
            componentFraction <= 0.125 &&
            componentWidth >= Math.max(Math.floor(gridWidth / 40), 4) &&
            componentHeight >= Math.max(Math.floor(gridHeight / 80), 2) &&
            aspectRatio >= 1.8 &&
            aspectRatio <= 7 &&
            fillRatio >= 0.45;
        if (!plausible) continue;
        const observation = {
            marker: markerName(label),
            componentPixels: count,
            sampledPixels,
            componentFraction,
            fillRatio,
            bounds: {
                left: minX * step,
                top: minY * step,
                right: Math.min((maxX + 1) * step, width),
                bottom: Math.min((maxY + 1) * step, height),
            },
        };
        if (!best || observation.componentPixels > best.componentPixels) best = observation;
    }
    return best;
}

export function summarizeMotion(ratios, movingThreshold = 0.0002) {
    if (!ratios.length) {
        return {
            comparedFramePairs: 0,
            movingFramePairs: 0,
            movingFrameFraction: 0,
            meanMotionRatio: 0,
            medianMotionRatio: 0,
            maximumMotionRatio: 0,
            longestFrozenRun: 0,
        };
    }
    const sorted = [...ratios].sort((left, right) => left - right);
    const movingFramePairs = ratios.filter((ratio) => ratio > movingThreshold).length;
    let longestFrozenRun = 0;
    let frozenRun = 0;
    for (const ratio of ratios) {
        if (ratio > movingThreshold) frozenRun = 0;
        else longestFrozenRun = Math.max(longestFrozenRun, ++frozenRun);
    }
    const middle = Math.floor(sorted.length / 2);
    const median = sorted.length % 2
        ? sorted[middle]
        : (sorted[middle - 1] + sorted[middle]) / 2;
    return {
        comparedFramePairs: ratios.length,
        movingFramePairs,
        movingFrameFraction: movingFramePairs / ratios.length,
        meanMotionRatio: ratios.reduce((sum, ratio) => sum + ratio, 0) / ratios.length,
        medianMotionRatio: median,
        maximumMotionRatio: sorted.at(-1),
        longestFrozenRun,
    };
}

export function analyzeSignal(values, sampleRate, frequencies = [660, 440]) {
    const count = Math.max(values.length, 1);
    let squares = 0;
    let peak = 0;
    let sum = 0;
    let clipped = 0;
    const amplitudes = {};
    for (const sample of values) {
        squares += sample * sample;
        sum += sample;
        peak = Math.max(peak, Math.abs(sample));
        if (Math.abs(sample) >= 0.999) clipped++;
    }
    for (const frequency of frequencies) {
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
    const rms = Math.sqrt(squares / count);
    return {
        sampleCount: values.length,
        rms,
        peak,
        dcOffset: sum / count,
        clippedSampleRatio: clipped / count,
        crestFactor: rms > 0 ? peak / rms : 0,
        amplitudes,
    };
}

export function evaluateReceiverReport(report) {
    const names = ['browser', 'tone', 'mixed', 'muted'];
    const markerSequenceValid = report.markerTransitions.length >= names.length &&
        new Set(report.markerTransitions.map(({ marker }) => marker)).size === names.length &&
        report.markerTransitions.every(({ marker }, index, transitions) => {
            if (index === 0) return names.includes(marker);
            const previous = names.indexOf(transitions[index - 1].marker);
            return marker === names[(previous + 1) % names.length];
        });
    const stagesComplete = names.every((name) => {
        const stage = report[name];
        return stage.observedFrames >= 8 &&
            stage.signal.sampleCount >= report.targetStageSamples &&
            stage.maximumInterFrameGapMs < 2500;
    });
    const signalsClean = names.every((name) => {
        const signal = report[name].signal;
        return Number.isFinite(signal.rms) &&
            Number.isFinite(signal.peak) && signal.peak <= 1 &&
            Number.isFinite(signal.crestFactor) && signal.crestFactor >= 0 &&
            signal.clippedSampleRatio < 0.001 &&
            Math.abs(signal.dcOffset) < 0.02;
    });
    const motionSustained = ['browser', 'tone', 'mixed'].every((name) => {
        const motion = report[name].motion;
        return motion.comparedFramePairs >= 5 &&
            motion.movingFrameFraction >= 0.4 &&
            motion.medianMotionRatio > 0.0002 &&
            motion.longestFrozenRun <= 4;
    });
    const browser660 = report.browser.signal.amplitudes['660Hz'];
    const browser440 = report.browser.signal.amplitudes['440Hz'];
    const tone440 = report.tone.signal.amplitudes['440Hz'];
    const tone660 = report.tone.signal.amplitudes['660Hz'];
    const mixed660 = report.mixed.signal.amplitudes['660Hz'];
    const mixed440 = report.mixed.signal.amplitudes['440Hz'];
    const activeRms = Math.max(report.browser.signal.rms, report.tone.signal.rms, report.mixed.signal.rms);
    const mixBalance = mixed440 > 0 && mixed660 / mixed440 >= 0.1 && mixed660 / mixed440 <= 10;
    return report.receiverMuted &&
        report.videoObservationMode === 'requestVideoFrameCallback' &&
        markerSequenceValid &&
        report.tracksAtStart.length === 2 &&
        report.tracksAtEnd.length === 2 &&
        report.trackCountsAtStart.audio === 1 &&
        report.trackCountsAtStart.video === 1 &&
        report.trackCountsAtEnd.audio === 1 &&
        report.trackCountsAtEnd.video === 1 &&
        report.liveTrackCountsAtStart.audio === 1 &&
        report.liveTrackCountsAtStart.video === 1 &&
        report.liveTrackCountsAtEnd.audio === 1 &&
        report.liveTrackCountsAtEnd.video === 1 &&
        report.receivedVideoWidth >= 640 &&
        report.receivedVideoHeight >= 360 &&
        report.videoTrackAtEnd.readyState === 'live' &&
        report.videoTrackAtEnd.enabled &&
        report.audioTrackAtEnd.readyState === 'live' &&
        report.audioTrackAtEnd.enabled &&
        report.trackEvents.ended === 0 &&
        report.trackEvents.mute === 0 &&
        report.trackEvents.unmute === 0 &&
        stagesComplete &&
        signalsClean &&
        motionSustained &&
        browser660 > 0.002 && browser660 > browser440 * 3 &&
        tone440 > 0.002 && tone440 > tone660 * 3 &&
        mixed660 > 0.001 && mixed440 > 0.001 && mixBalance &&
        report.muted.signal.rms < activeRms * 0.25 &&
        report.muted.signal.rms < 0.01 &&
        report.audioCallback.count > 0 &&
        report.audioCallback.maximumGapMs < 1500;
}

export function browserOracleSource() {
    return [
        markerIndex,
        markerName,
        classifyMarkerPixels,
        summarizeMotion,
        analyzeSignal,
        evaluateReceiverReport,
    ].map((fn) => fn.toString()).join('\n');
}
