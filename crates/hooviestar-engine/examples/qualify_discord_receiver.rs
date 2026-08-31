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
        find_window_containing, quiet_enough, readback_bgra8, start_window_capture,
        summarize_motion, window_process_id,
    };

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct StageReport {
        marker: &'static str,
        observed_frames: usize,
        maximum_motion_ratio: f64,
        maximum_inter_frame_gap_ms: f64,
        marker_component_fraction: f64,
        motion: support::windows::MotionMetrics,
        signal: support::windows::SignalMetrics,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TransportReport {
        schema_version: u32,
        qualification_run_id: String,
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
    let qualification_run_id = value("--run-id").unwrap_or("unlinked").to_string();
    let duration_seconds: u64 = value("--duration")
        .unwrap_or("96")
        .parse()
        .map_err(|_| "--duration must be an integer".to_string())?;
    if duration_seconds < 32 {
        return Err("--duration must be at least 32 seconds".into());
    }
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

    let mut current_marker = None;
    let mut marker_since = std::time::Instant::now();
    let mut previous = None::<BgraFrame>;
    let mut previous_frame_time = None::<std::time::Instant>;
    let mut motion = HashMap::<Marker, Vec<f64>>::new();
    let mut maximum_inter_frame_gap = HashMap::<Marker, f64>::new();
    let mut marker_component_fraction = HashMap::<Marker, f64>::new();
    let mut frames = HashMap::<Marker, usize>::new();
    let mut samples = HashMap::<Marker, Vec<f64>>::new();
    let mut current_samples = Vec::new();
    let target_stage_samples = SAMPLE_RATE as usize * 4;
    let deadline = std::time::Instant::now() + Duration::from_secs(duration_seconds);
    while std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
        if let Some(captured) = video.take_latest() {
            let frame = readback_bgra8(&device, &captured.texture)?;
            if let Some(observation) = frame.marker_observation() {
                let marker = observation.marker;
                if current_marker != Some(marker) {
                    if let Some(previous_marker) = current_marker
                        && samples
                            .get(&previous_marker)
                            .is_none_or(|best| current_samples.len() > best.len())
                    {
                        samples.insert(previous_marker, std::mem::take(&mut current_samples));
                    }
                    current_samples.clear();
                    marker_since = std::time::Instant::now();
                    previous = None;
                    previous_frame_time = None;
                }
                current_marker = Some(marker);
                *frames.entry(marker).or_default() += 1;
                marker_component_fraction
                    .entry(marker)
                    .and_modify(|minimum| *minimum = minimum.min(observation.component_fraction))
                    .or_insert(observation.component_fraction);
                let now = std::time::Instant::now();
                if let Some(older_time) = previous_frame_time {
                    let gap_ms = now.duration_since(older_time).as_secs_f64() * 1000.0;
                    maximum_inter_frame_gap
                        .entry(marker)
                        .and_modify(|maximum| *maximum = maximum.max(gap_ms))
                        .or_insert(gap_ms);
                }
                if let Some(older) = previous.replace(frame.clone()) {
                    let ratio = older.motion_ratio(&frame);
                    motion.entry(marker).or_default().push(ratio);
                }
                previous_frame_time = Some(now);
            }
        }
        if current_marker.is_none() {
            continue;
        }
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
        let remaining = target_stage_samples.saturating_sub(current_samples.len());
        for index in 0..SAMPLE_RATE / 50 {
            let frame = audio.pop();
            if index < remaining as u32 {
                current_samples.push((f64::from(frame[0]) + f64::from(frame[1])) * 0.5);
            }
        }
    }
    if let Some(marker) = current_marker
        && samples
            .get(&marker)
            .is_none_or(|best| current_samples.len() > best.len())
    {
        samples.insert(marker, current_samples);
    }
    audio.shutdown();

    let stage = |marker: Marker, name: &'static str| {
        let motion = summarize_motion(
            motion.get(&marker).map(Vec::as_slice).unwrap_or_default(),
            0.0002,
        );
        StageReport {
            marker: name,
            observed_frames: frames.get(&marker).copied().unwrap_or_default(),
            maximum_motion_ratio: motion.maximum_motion_ratio,
            maximum_inter_frame_gap_ms: maximum_inter_frame_gap
                .get(&marker)
                .copied()
                .unwrap_or(f64::MAX),
            marker_component_fraction: marker_component_fraction
                .get(&marker)
                .copied()
                .unwrap_or_default(),
            motion,
            signal: analyze_signal(
                samples.get(&marker).map(Vec::as_slice).unwrap_or_default(),
                &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
            ),
        }
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
    let stages_complete = [&browser, &tone, &mixed, &muted].iter().all(|stage| {
        stage.observed_frames >= 8
            && stage.signal.sample_count >= target_stage_samples
            && stage.maximum_inter_frame_gap_ms < 2_500.0
    });
    let motion_sustained = [&browser, &tone, &mixed]
        .iter()
        .all(|stage| stage.motion.sustained(5, 0.4, 0.0002, 4));
    let mixed_balance = mixed.signal.amplitude(BROWSER_FREQUENCY_HZ)
        / mixed
            .signal
            .amplitude(TONE_FREQUENCY_HZ)
            .max(f64::MIN_POSITIVE);
    let passed = stages_complete
        && motion_sustained
        && browser.signal.amplitude(BROWSER_FREQUENCY_HZ) > 0.002
        && browser.signal.amplitude(BROWSER_FREQUENCY_HZ)
            > browser.signal.amplitude(TONE_FREQUENCY_HZ) * 3.0
        && tone.signal.amplitude(TONE_FREQUENCY_HZ) > 0.002
        && tone.signal.amplitude(TONE_FREQUENCY_HZ)
            > tone.signal.amplitude(BROWSER_FREQUENCY_HZ) * 3.0
        && mixed.signal.amplitude(BROWSER_FREQUENCY_HZ) > 0.001
        && mixed.signal.amplitude(TONE_FREQUENCY_HZ) > 0.001
        && (0.1..=10.0).contains(&mixed_balance)
        && [&browser, &tone, &mixed, &muted]
            .iter()
            .all(|stage| stage.signal.dc_offset.abs() < 0.02)
        && quiet_enough(&muted.signal, active_rms, 0.25, 0.01);
    let report = TransportReport {
        schema_version: 2,
        qualification_run_id,
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
