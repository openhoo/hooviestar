#[cfg(target_os = "windows")]
mod support;

#[cfg(target_os = "windows")]
fn main() {
    if let Err(error) = windows_main() {
        eprintln!("Discord receiver qualification failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
fn windows_main() -> Result<(), String> {
    use std::{collections::HashMap, path::PathBuf, thread, time::Duration};

    use hooviestar_engine::audio::{SAMPLE_RATE, windows_runtime::ProcessAudioCapture};
    use serde::Serialize;
    use support::windows::{
        BROWSER_FREQUENCY_HZ, BgraFrame, Marker, TONE_FREQUENCY_HZ, analyze_signal,
        find_window_containing, readback_bgra8, start_window_capture, window_process_id,
    };

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct StageReport {
        marker: &'static str,
        observed_frames: usize,
        maximum_motion_ratio: f64,
        signal: support::windows::SignalMetrics,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TransportReport {
        passed: bool,
        transport: String,
        captured_window_title: String,
        captured_process_id: u32,
        browser: StageReport,
        tone: StageReport,
        mixed: StageReport,
        muted: StageReport,
    }

    let arguments: Vec<String> = std::env::args().collect();
    let value = |name: &str| {
        arguments
            .windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].as_str())
    };
    let title_fragment = value("--window-title-contains").unwrap_or("Discord");
    let transport = value("--transport").unwrap_or("Discord");
    let duration_seconds: u64 = value("--duration")
        .unwrap_or("96")
        .parse()
        .map_err(|_| "--duration must be an integer".to_string())?;
    let report_path = PathBuf::from(value("--report").unwrap_or("discord-receiver-report.json"));

    let (window_title, hwnd) = find_window_containing(title_fragment)?;
    let process_id = window_process_id(hwnd);
    if process_id == 0 {
        return Err("Discord receiver window has no owning process".into());
    }
    let (device, video) = start_window_capture(hwnd).map_err(|error| error.to_string())?;
    let audio = ProcessAudioCapture::start(process_id)?;
    println!(
        "Capturing {transport} receiver window {window_title:?} (PID {process_id}) for {duration_seconds}s. Keep the Hooviestar stream visible and audible."
    );

    let markers = [Marker::Browser, Marker::Tone, Marker::Mixed, Marker::Muted];
    let mut current_marker = None;
    let mut marker_since = std::time::Instant::now();
    let mut previous = HashMap::<Marker, BgraFrame>::new();
    let mut motion = HashMap::<Marker, f64>::new();
    let mut frames = HashMap::<Marker, usize>::new();
    let mut samples = HashMap::<Marker, Vec<f64>>::new();
    let target_stage_samples = SAMPLE_RATE as usize * 4;
    let deadline = std::time::Instant::now() + Duration::from_secs(duration_seconds);
    while std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
        if let Some(captured) = video.take_latest() {
            let frame = readback_bgra8(&device, &captured.texture)?;
            if let Some(marker) = frame.marker() {
                if current_marker != Some(marker) {
                    marker_since = std::time::Instant::now();
                    // If measurement began near a stage boundary, discard its
                    // short partial segment on the next occurrence. Once four
                    // contiguous seconds exist, freeze the bucket. Never join
                    // independent Discord cycles: phase discontinuities could
                    // cancel a real tone in the frequency oracle.
                    if samples.get(&marker).map_or(0, Vec::len) < target_stage_samples {
                        samples.insert(marker, Vec::with_capacity(target_stage_samples));
                    }
                }
                current_marker = Some(marker);
                *frames.entry(marker).or_default() += 1;
                if let Some(older) = previous.insert(marker, frame.clone()) {
                    let ratio = older.motion_ratio(&frame);
                    motion
                        .entry(marker)
                        .and_modify(|maximum| *maximum = maximum.max(ratio))
                        .or_insert(ratio);
                }
            }
        }
        let Some(marker) = current_marker else {
            continue;
        };
        // Scene commands, the source-management thread, Discord encoding,
        // and receiver playback do not transition on the same millisecond.
        // Keep visual transition frames, but exclude the first second from
        // each audio bucket so old-stage tail cannot fake cross-frequency
        // leakage or inflate the muted reference.
        if marker_since.elapsed() < Duration::from_secs(1) {
            for _ in 0..SAMPLE_RATE / 50 {
                let _ = audio.pop();
            }
            continue;
        }
        let bucket = samples.entry(marker).or_default();
        let remaining = target_stage_samples.saturating_sub(bucket.len());
        for index in 0..SAMPLE_RATE / 50 {
            let frame = audio.pop();
            if index < remaining as u32 {
                bucket.push((f64::from(frame[0]) + f64::from(frame[1])) * 0.5);
            }
        }
    }
    audio.shutdown();

    let stage = |marker: Marker, name: &'static str| StageReport {
        marker: name,
        observed_frames: frames.get(&marker).copied().unwrap_or_default(),
        maximum_motion_ratio: motion.get(&marker).copied().unwrap_or_default(),
        signal: analyze_signal(
            samples.get(&marker).map(Vec::as_slice).unwrap_or_default(),
            &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
        ),
    };
    let browser = stage(Marker::Browser, "browser");
    let tone = stage(Marker::Tone, "tone");
    let mixed = stage(Marker::Mixed, "mixed");
    let muted = stage(Marker::Muted, "muted");
    let active_rms = browser
        .signal
        .rms
        .max(tone.signal.rms)
        .max(mixed.signal.rms);
    let passed = markers
        .iter()
        .all(|marker| frames.get(marker).copied().unwrap_or_default() >= 3)
        && browser.maximum_motion_ratio > 0.0002
        && tone.maximum_motion_ratio > 0.0002
        && mixed.maximum_motion_ratio > 0.0002
        && browser.signal.amplitude(BROWSER_FREQUENCY_HZ) > 0.002
        && browser.signal.amplitude(BROWSER_FREQUENCY_HZ)
            > browser.signal.amplitude(TONE_FREQUENCY_HZ) * 3.0
        && tone.signal.amplitude(TONE_FREQUENCY_HZ) > 0.002
        && tone.signal.amplitude(TONE_FREQUENCY_HZ)
            > tone.signal.amplitude(BROWSER_FREQUENCY_HZ) * 3.0
        && mixed.signal.amplitude(BROWSER_FREQUENCY_HZ) > 0.001
        && mixed.signal.amplitude(TONE_FREQUENCY_HZ) > 0.001
        && muted.signal.rms < active_rms * 0.25;
    let report = TransportReport {
        passed,
        transport: transport.to_string(),
        captured_window_title: window_title,
        captured_process_id: process_id,
        browser,
        tone,
        mixed,
        muted,
    };
    if let Some(parent) = report_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        &report_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())?;
    println!("{transport} receiver report: {}", report_path.display());
    if passed {
        Ok(())
    } else {
        Err(format!(
            "{transport} video/audio assertions failed; inspect {}",
            report_path.display()
        ))
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("transport receiver qualification requires Windows");
    std::process::exit(2);
}
