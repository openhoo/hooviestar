#[cfg(target_os = "windows")]
mod support;

#[cfg(target_os = "windows")]
fn main() {
    if let Err(error) = windows_main() {
        eprintln!("Windows pipeline qualification failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
fn windows_main() -> Result<(), String> {
    use std::{
        collections::HashSet,
        path::PathBuf,
        sync::mpsc::RecvTimeoutError,
        thread,
        time::{Duration, Instant},
    };

    use hooviestar_engine::{
        EngineCommand, EngineEvent, EngineHandle, NativeSurfaceKind, NativeSurfaces, OutputConfig,
        audio::windows_runtime::ProcessAudioCapture,
        discovery::windows::{mute_audio_session, restore_audio_session},
        project::{Source, TextAlign, Transform},
    };
    use serde::Serialize;
    use support::windows::{
        BROWSER_FREQUENCY_HZ, BROWSER_TITLE, Marker, PREVIEW_TITLE, PROGRAM_TITLE,
        TONE_FREQUENCY_HZ, TestWindow, audio_binding_for_pid, audio_binding_for_tree,
        measure_process_audio, process_tree, start_window_capture, wait_for_frame, wait_for_marker,
        wait_for_window_binding, window_process_id,
    };
    use uuid::Uuid;

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct VisualReport {
        program_mapped_offscreen: bool,
        program_capture_width: u32,
        program_capture_height: u32,
        preview_capture_width: u32,
        preview_capture_height: u32,
        browser_motion_ratio: f64,
        browser_scene_marker: bool,
        tone_scene_marker: bool,
        mixed_scene_marker: bool,
        muted_scene_marker: bool,
        output_resize_applied: bool,
        output_resize_kept_rendering: bool,
        output_resize_restored: bool,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct QualificationReport {
        passed: bool,
        visual: VisualReport,
        browser_only: support::windows::SignalMetrics,
        tone_only: support::windows::SignalMetrics,
        mixed: support::windows::SignalMetrics,
        muted: support::windows::SignalMetrics,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TransportAudioPreflight {
        passed: bool,
        browser: support::windows::SignalMetrics,
        tone: support::windows::SignalMetrics,
        mixed: support::windows::SignalMetrics,
        muted: support::windows::SignalMetrics,
    }

    let arguments: Vec<String> = std::env::args().collect();
    let value = |name: &str| {
        arguments
            .windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].as_str())
    };
    let tone_pid: u32 = value("--tone-pid")
        .ok_or_else(|| "missing --tone-pid".to_string())?
        .parse()
        .map_err(|_| "--tone-pid must be an integer".to_string())?;
    let hold_seconds: u64 = value("--hold-seconds")
        .unwrap_or("0")
        .parse()
        .map_err(|_| "--hold-seconds must be an integer".to_string())?;
    let transport_label = value("--transport-label").unwrap_or("Discord");
    let program_onscreen = arguments
        .iter()
        .any(|argument| argument == "--program-onscreen");
    let system_audio_transport = arguments
        .iter()
        .any(|argument| argument == "--system-audio-transport");
    let program_topmost_gate = value("--program-topmost-gate").map(PathBuf::from);
    let report_path = PathBuf::from(value("--report").unwrap_or("windows-pipeline-report.json"));

    let (browser_hwnd, browser_window_binding) =
        wait_for_window_binding(BROWSER_TITLE, Duration::from_secs(30))?;
    let browser_root = window_process_id(browser_hwnd);
    let browser_audio_binding = wait_for_audio_binding(Duration::from_secs(30), || {
        audio_binding_for_tree(&process_tree(browser_root)?)
    })?;
    let tone_audio_binding =
        wait_for_audio_binding(Duration::from_secs(30), || audio_binding_for_pid(tone_pid))?;

    let program = TestWindow::create(PROGRAM_TITLE, 1280, 720, !program_onscreen)?;
    let preview = TestWindow::create(PREVIEW_TITLE, 1280, 720, true)?;
    if program_onscreen {
        program.assert_captureable_onscreen()?;
    } else {
        program.assert_discord_captureable_offscreen()?;
    }

    let engine = EngineHandle::start(
        NativeSurfaces {
            studio: 0,
            program: program.raw(),
            preview: preview.raw(),
            display: 0,
            kind: NativeSurfaceKind::Win32,
            program_width: 1280,
            program_height: 720,
            preview_width: 1280,
            preview_height: 720,
        },
        OutputConfig::default(),
    )
    .map_err(|error| error.to_string())?;
    let engine_events = engine.take_events().map_err(|error| error.to_string())?;

    let scenes = engine.snapshot().scenes;
    if scenes.len() < 3 {
        return Err("qualification requires three default scenes".into());
    }
    let browser_scene = scenes[0].id;
    let tone_scene = scenes[1].id;
    let mixed_scene = scenes[2].id;
    let muted_scene = Uuid::new_v4();
    engine
        .command(EngineCommand::AddScene {
            scene_id: muted_scene,
            name: "Qualification Muted".into(),
        })
        .map_err(|error| error.to_string())?;

    let browser_video_id = Uuid::new_v4();
    let browser_audio_id = Uuid::new_v4();
    let tone_audio_id = Uuid::new_v4();
    engine
        .command(EngineCommand::AddSource {
            source: Source::Window {
                id: browser_video_id,
                name: "Browser video fixture".into(),
                binding: browser_window_binding,
            },
        })
        .map_err(|error| error.to_string())?;
    engine
        .command(EngineCommand::AddSource {
            source: Source::ApplicationAudio {
                id: browser_audio_id,
                name: "Browser video audio".into(),
                binding: browser_audio_binding.clone(),
                volume: 1.0,
                muted: false,
            },
        })
        .map_err(|error| error.to_string())?;
    engine
        .command(EngineCommand::AddSource {
            source: Source::ApplicationAudio {
                id: tone_audio_id,
                name: "Independent tone".into(),
                binding: tone_audio_binding.clone(),
                volume: 1.0,
                muted: true,
            },
        })
        .map_err(|error| error.to_string())?;

    let stages = [
        (
            browser_scene,
            Marker::Browser,
            "#ff00ff",
            "BROWSER VIDEO + AUDIO",
        ),
        (tone_scene, Marker::Tone, "#00ffff", "BROWSER VIDEO + TONE"),
        (mixed_scene, Marker::Mixed, "#ffff00", "BROWSER PIP + MIX"),
        (muted_scene, Marker::Muted, "#0000ff", "MUTED REFERENCE"),
    ];
    for (index, (scene_id, _, color, label)) in stages.iter().enumerate() {
        let marker_id = Uuid::new_v4();
        engine
            .command(EngineCommand::AddSource {
                source: Source::Text {
                    id: marker_id,
                    name: format!("Qualification marker {index}"),
                    text: (*label).into(),
                    font_family: "Segoe UI".into(),
                    font_size_px: 22.0,
                    font_weight: 700,
                    color: "#000000".into(),
                    background_color: (*color).into(),
                    align: TextAlign::Center,
                },
            })
            .map_err(|error| error.to_string())?;
        if *scene_id != muted_scene {
            let transform = match index {
                0 => Transform::default(),
                1 => Transform {
                    x: 120.0,
                    y: 70.0,
                    width: 1040.0,
                    height: 585.0,
                    crop_left: 20.0,
                    crop_right: 20.0,
                    ..Transform::default()
                },
                _ => Transform {
                    x: 640.0,
                    y: 330.0,
                    width: 600.0,
                    height: 337.5,
                    rotation_degrees: -3.0,
                    opacity: 0.92,
                    ..Transform::default()
                },
            };
            engine
                .command(EngineCommand::AddSceneItem {
                    scene_id: *scene_id,
                    item_id: Uuid::new_v4(),
                    source_id: browser_video_id,
                    transform,
                })
                .map_err(|error| error.to_string())?;
        }
        engine
            .command(EngineCommand::AddSceneItem {
                scene_id: *scene_id,
                item_id: Uuid::new_v4(),
                source_id: marker_id,
                transform: Transform {
                    x: 24.0,
                    y: 24.0,
                    width: 360.0,
                    height: 96.0,
                    ..Transform::default()
                },
            })
            .map_err(|error| error.to_string())?;
    }

    let (program_device, program_capture) =
        start_window_capture(program.raw()).map_err(|error| error.to_string())?;
    let (preview_device, preview_capture) =
        start_window_capture(preview.raw()).map_err(|error| error.to_string())?;

    set_stage(
        &engine,
        browser_scene,
        browser_audio_id,
        tone_audio_id,
        1.0,
        true,
    )?;
    let browser_frame = wait_for_marker(
        &program_capture,
        &program_device,
        Marker::Browser,
        Duration::from_secs(20),
    )?;
    thread::sleep(Duration::from_millis(650));
    let browser_frame_later = wait_for_marker(
        &program_capture,
        &program_device,
        Marker::Browser,
        Duration::from_secs(5),
    )?;
    let browser_motion_ratio = browser_frame.motion_ratio(&browser_frame_later);
    let preview_frame = wait_for_marker(
        &preview_capture,
        &preview_device,
        Marker::Browser,
        Duration::from_secs(5),
    )?;

    set_stage(
        &engine,
        tone_scene,
        browser_audio_id,
        tone_audio_id,
        0.0,
        false,
    )?;
    let tone_marker = wait_for_marker(
        &program_capture,
        &program_device,
        Marker::Tone,
        Duration::from_secs(5),
    )?;
    set_stage(
        &engine,
        mixed_scene,
        browser_audio_id,
        tone_audio_id,
        0.5,
        false,
    )?;
    let mixed_marker = wait_for_marker(
        &program_capture,
        &program_device,
        Marker::Mixed,
        Duration::from_secs(5),
    )?;

    drain_engine_events(&engine_events)?;
    engine
        .command(EngineCommand::SetOutputConfig {
            output: OutputConfig {
                width: 1920,
                height: 1080,
                fps: 60,
                background: "#101418".into(),
            },
        })
        .map_err(|error| error.to_string())?;
    let output_resize_applied = wait_for_resize_result(&engine_events, Duration::from_secs(10))?;
    let _ = program_capture.take_latest();
    let resized = wait_for_marker(
        &program_capture,
        &program_device,
        Marker::Mixed,
        Duration::from_secs(10),
    )?;
    drain_engine_events(&engine_events)?;
    engine
        .command(EngineCommand::SetOutputConfig {
            output: OutputConfig::default(),
        })
        .map_err(|error| error.to_string())?;
    let output_resize_restored = if output_resize_applied {
        wait_for_resize_result(&engine_events, Duration::from_secs(10))?
    } else {
        false
    };

    // The mixer creates the process render session before EngineHandle::start
    // returns. Capturing this process isolates Hooviestar's mixed output from
    // the two source sessions, which are sibling processes launched by the
    // PowerShell orchestrator.
    let mixed_output = ProcessAudioCapture::start(std::process::id())?;
    set_stage(
        &engine,
        browser_scene,
        browser_audio_id,
        tone_audio_id,
        1.0,
        true,
    )?;
    thread::sleep(Duration::from_millis(900));
    let browser_only = measure_process_audio(
        &mixed_output,
        Duration::from_secs(1),
        &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
    );
    set_stage(
        &engine,
        tone_scene,
        browser_audio_id,
        tone_audio_id,
        0.0,
        false,
    )?;
    thread::sleep(Duration::from_millis(900));
    let tone_only = measure_process_audio(
        &mixed_output,
        Duration::from_secs(1),
        &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
    );
    set_stage(
        &engine,
        mixed_scene,
        browser_audio_id,
        tone_audio_id,
        0.5,
        false,
    )?;
    thread::sleep(Duration::from_millis(900));
    let mixed = measure_process_audio(
        &mixed_output,
        Duration::from_secs(1),
        &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
    );
    set_stage(
        &engine,
        muted_scene,
        browser_audio_id,
        tone_audio_id,
        0.0,
        true,
    )?;
    let muted_marker = wait_for_marker(
        &program_capture,
        &program_device,
        Marker::Muted,
        Duration::from_secs(5),
    )?;
    thread::sleep(Duration::from_millis(900));
    let muted = measure_process_audio(
        &mixed_output,
        Duration::from_secs(1),
        &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
    );
    mixed_output.shutdown();
    drop(mixed_output);

    let visual = VisualReport {
        program_mapped_offscreen: !program_onscreen,
        program_capture_width: browser_frame.width,
        program_capture_height: browser_frame.height,
        preview_capture_width: preview_frame.width,
        preview_capture_height: preview_frame.height,
        browser_motion_ratio,
        browser_scene_marker: browser_frame.marker() == Some(Marker::Browser),
        tone_scene_marker: tone_marker.marker() == Some(Marker::Tone),
        mixed_scene_marker: mixed_marker.marker() == Some(Marker::Mixed),
        muted_scene_marker: muted_marker.marker() == Some(Marker::Muted),
        output_resize_applied,
        output_resize_kept_rendering: resized.marker() == Some(Marker::Mixed),
        output_resize_restored,
    };
    let browser_amplitude = browser_only.amplitude(BROWSER_FREQUENCY_HZ);
    let browser_leak = browser_only.amplitude(TONE_FREQUENCY_HZ);
    let tone_amplitude = tone_only.amplitude(TONE_FREQUENCY_HZ);
    let tone_leak = tone_only.amplitude(BROWSER_FREQUENCY_HZ);
    let active_rms = browser_only.rms.max(tone_only.rms).max(mixed.rms);
    let passed = visual.browser_motion_ratio > 0.0005
        && visual.browser_scene_marker
        && visual.tone_scene_marker
        && visual.mixed_scene_marker
        && visual.muted_scene_marker
        && visual.output_resize_applied
        && visual.output_resize_kept_rendering
        && visual.output_resize_restored
        && browser_amplitude > 0.005
        && browser_amplitude > browser_leak * 6.0
        && tone_amplitude > 0.005
        && tone_amplitude > tone_leak * 6.0
        && mixed.amplitude(BROWSER_FREQUENCY_HZ) > 0.002
        && mixed.amplitude(TONE_FREQUENCY_HZ) > 0.002
        && mixed.peak <= 0.92
        && muted.rms < active_rms * 0.12;
    let report = QualificationReport {
        passed,
        visual,
        browser_only,
        tone_only,
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
    println!("native qualification report: {}", report_path.display());
    if !passed {
        // Stop qualification WGC readers before the engine destroys the
        // Program/Preview swapchains they are observing. QXL/basic-display
        // drivers can otherwise fault in the outstanding frame callback.
        drop(preview_capture);
        drop(program_capture);
        drop(preview_device);
        drop(program_device);
        engine.shutdown().map_err(|error| error.to_string())?;
        return Err(format!(
            "native scene/video/audio assertions failed; inspect {}",
            report_path.display()
        ));
    }

    let mut hold_browser_audio_id = browser_audio_id;
    let mut hold_tone_audio_id = tone_audio_id;
    let mut session_restores = Vec::new();
    let mut mute_journal = None;
    if hold_seconds > 0 && system_audio_transport {
        let fixture_root = report_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("system-audio-fixtures");
        std::fs::create_dir_all(&fixture_root).map_err(|error| error.to_string())?;
        let browser_tone_path = fixture_root.join("browser-660hz.wav");
        let independent_tone_path = fixture_root.join("tone-440hz.wav");
        write_tone_wav(&browser_tone_path, BROWSER_FREQUENCY_HZ, 0.12)?;
        write_tone_wav(&independent_tone_path, TONE_FREQUENCY_HZ, 0.20)?;

        hold_browser_audio_id = Uuid::new_v4();
        hold_tone_audio_id = Uuid::new_v4();
        for (source_id, name, path) in [
            (
                hold_browser_audio_id,
                "Transport browser tone",
                browser_tone_path,
            ),
            (
                hold_tone_audio_id,
                "Transport independent tone",
                independent_tone_path,
            ),
        ] {
            engine
                .command(EngineCommand::AddSource {
                    source: Source::Media {
                        id: source_id,
                        name: name.into(),
                        path: path.to_string_lossy().into_owned(),
                        looped: true,
                        continue_when_hidden: true,
                        restart_on_show: false,
                        volume: 1.0,
                        muted: false,
                    },
                })
                .map_err(|error| error.to_string())?;
            engine
                .command(EngineCommand::SetMediaPlaying {
                    source_id,
                    playing: true,
                })
                .map_err(|error| error.to_string())?;
        }
        engine
            .command(EngineCommand::RemoveSource {
                source_id: browser_audio_id,
            })
            .map_err(|error| error.to_string())?;
        engine
            .command(EngineCommand::RemoveSource {
                source_id: tone_audio_id,
            })
            .map_err(|error| error.to_string())?;

        let transport_ids = HashSet::from([hold_browser_audio_id, hold_tone_audio_id]);
        let mut audible_ids = HashSet::new();
        let readiness_deadline = Instant::now() + Duration::from_secs(20);
        while audible_ids.len() < transport_ids.len() && Instant::now() < readiness_deadline {
            match engine_events.recv_timeout(Duration::from_millis(250)) {
                Ok(event) => {
                    if let EngineEvent::Levels { entries } = &event {
                        for entry in entries {
                            if transport_ids.contains(&entry.source_id) && entry.rms > 0.005 {
                                audible_ids.insert(entry.source_id);
                            }
                        }
                    }
                    println!("qualification engine event: {event:?}");
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("engine event stream disconnected during media readiness".into());
                }
            }
        }
        if audible_ids.len() != transport_ids.len() {
            return Err(format!(
                "transport media did not become audible: {}/{} sources ready",
                audible_ids.len(),
                transport_ids.len()
            ));
        }

        let transport_output = ProcessAudioCapture::start(std::process::id())?;
        set_stage(
            &engine,
            browser_scene,
            hold_browser_audio_id,
            hold_tone_audio_id,
            1.0,
            true,
        )?;
        thread::sleep(Duration::from_millis(900));
        let transport_browser = measure_process_audio(
            &transport_output,
            Duration::from_secs(1),
            &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
        );
        set_stage(
            &engine,
            tone_scene,
            hold_browser_audio_id,
            hold_tone_audio_id,
            0.0,
            false,
        )?;
        thread::sleep(Duration::from_millis(900));
        let transport_tone = measure_process_audio(
            &transport_output,
            Duration::from_secs(1),
            &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
        );
        set_stage(
            &engine,
            mixed_scene,
            hold_browser_audio_id,
            hold_tone_audio_id,
            0.5,
            false,
        )?;
        thread::sleep(Duration::from_millis(900));
        let transport_mixed = measure_process_audio(
            &transport_output,
            Duration::from_secs(1),
            &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
        );
        set_stage(
            &engine,
            muted_scene,
            hold_browser_audio_id,
            hold_tone_audio_id,
            0.0,
            true,
        )?;
        thread::sleep(Duration::from_millis(900));
        let transport_muted = measure_process_audio(
            &transport_output,
            Duration::from_secs(1),
            &[BROWSER_FREQUENCY_HZ, TONE_FREQUENCY_HZ],
        );
        transport_output.shutdown();
        drop(transport_output);
        let transport_active_rms = transport_browser
            .rms
            .max(transport_tone.rms)
            .max(transport_mixed.rms);
        let transport_passed = transport_browser.amplitude(BROWSER_FREQUENCY_HZ) > 0.005
            && transport_browser.amplitude(BROWSER_FREQUENCY_HZ)
                > transport_browser.amplitude(TONE_FREQUENCY_HZ) * 6.0
            && transport_tone.amplitude(TONE_FREQUENCY_HZ) > 0.005
            && transport_tone.amplitude(TONE_FREQUENCY_HZ)
                > transport_tone.amplitude(BROWSER_FREQUENCY_HZ) * 6.0
            && transport_mixed.amplitude(BROWSER_FREQUENCY_HZ) > 0.002
            && transport_mixed.amplitude(TONE_FREQUENCY_HZ) > 0.002
            && transport_mixed.peak <= 0.92
            && transport_muted.rms < transport_active_rms * 0.12;
        let transport_preflight = TransportAudioPreflight {
            passed: transport_passed,
            browser: transport_browser,
            tone: transport_tone,
            mixed: transport_mixed,
            muted: transport_muted,
        };
        let transport_report_path = fixture_root.join("preflight.json");
        std::fs::write(
            &transport_report_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&transport_preflight)
                    .map_err(|error| error.to_string())?
            ),
        )
        .map_err(|error| error.to_string())?;
        if !transport_passed {
            return Err(format!(
                "system-audio transport preflight failed; inspect {}",
                transport_report_path.display()
            ));
        }

        let journal = fixture_root.join("source-session-restore.json");
        let browser_restore = mute_audio_session(&browser_audio_binding, &journal)?;
        match mute_audio_session(&tone_audio_binding, &journal) {
            Ok(tone_restore) => {
                session_restores.push(browser_restore);
                session_restores.push(tone_restore);
                mute_journal = Some(journal);
            }
            Err(error) => {
                let _ = restore_audio_session(&browser_restore);
                return Err(error);
            }
        }
        println!(
            "{transport_label} system-audio isolation active and measured: source sessions muted, generated media mixed only by Hooviestar"
        );
    }

    let hold_result: Result<(), String> = if hold_seconds > 0 {
        println!(
            "native checks passed; share {PROGRAM_TITLE:?} through {transport_label} now. Cycling four measured stages for {hold_seconds}s"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(hold_seconds);
        let mut index = 0usize;
        while std::time::Instant::now() < deadline {
            let (scene, browser_volume, tone_muted) = match index % 4 {
                0 => (browser_scene, 1.0, true),
                1 => (tone_scene, 0.0, false),
                2 => (mixed_scene, 0.5, false),
                _ => (muted_scene, 0.0, true),
            };
            set_stage(
                &engine,
                scene,
                hold_browser_audio_id,
                hold_tone_audio_id,
                browser_volume,
                tone_muted,
            )?;
            if program_onscreen {
                if program_topmost_gate
                    .as_ref()
                    .is_some_and(|path| path.exists())
                {
                    program.release_topmost()?;
                } else {
                    program.raise_topmost()?;
                }
            }
            println!("{transport_label} stage: {:?}", stages[index % 4].1);
            thread::sleep(Duration::from_secs(8));
            index += 1;
        }
        Ok(())
    } else {
        Ok(())
    };
    let mut restore_error = None;
    for entry in session_restores.iter().rev() {
        if let Err(error) = restore_audio_session(entry) {
            restore_error.get_or_insert(error);
        }
    }
    if let Some(journal) = mute_journal {
        let _ = std::fs::remove_file(journal);
    }
    hold_result?;
    if let Some(error) = restore_error {
        return Err(format!("source-session restore failed: {error}"));
    }

    // A final frame proves the renderer survived the full hold/cycle period.
    let _ = wait_for_frame(&program_capture, &program_device, Duration::from_secs(3))?;
    drop(preview_capture);
    drop(program_capture);
    drop(preview_device);
    drop(program_device);
    engine.shutdown().map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn drain_engine_events(
    events: &std::sync::mpsc::Receiver<hooviestar_engine::EngineEvent>,
) -> Result<(), String> {
    loop {
        match events.try_recv() {
            Ok(event) => println!("qualification engine event: {event:?}"),
            Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(()),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("engine event stream disconnected".into());
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn wait_for_resize_result(
    events: &std::sync::mpsc::Receiver<hooviestar_engine::EngineEvent>,
    timeout: std::time::Duration,
) -> Result<bool, String> {
    use hooviestar_engine::{EngineEvent, engine::DeviceRecoveryPhase};

    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match events.recv_timeout(remaining.min(std::time::Duration::from_millis(250))) {
            Ok(event) => {
                let resize_result = match &event {
                    EngineEvent::DeviceRecovery {
                        phase: DeviceRecoveryPhase::Succeeded,
                        ..
                    } => Some(true),
                    EngineEvent::DeviceRecovery {
                        phase: DeviceRecoveryPhase::Failed,
                        ..
                    } => Some(false),
                    _ => None,
                };
                println!("qualification engine event: {event:?}");
                if let Some(succeeded) = resize_result {
                    return Ok(succeeded);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("engine event stream disconnected during output resize".into());
            }
        }
    }
    Ok(false)
}

#[cfg(target_os = "windows")]
fn write_tone_wav(path: &std::path::Path, frequency_hz: f64, amplitude: f64) -> Result<(), String> {
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;
    const BITS_PER_SAMPLE: u16 = 16;
    const DURATION_SECONDS: u32 = 8;
    let frames = SAMPLE_RATE * DURATION_SECONDS;
    let block_align = CHANNELS * (BITS_PER_SAMPLE / 8);
    let data_size = frames * u32::from(block_align);
    let mut bytes = Vec::with_capacity(44 + data_size as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&CHANNELS.to_le_bytes());
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * u32::from(block_align)).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for frame in 0..frames {
        let phase =
            std::f64::consts::TAU * frequency_hz * f64::from(frame) / f64::from(SAMPLE_RATE);
        let sample = (phase.sin() * amplitude * f64::from(i16::MAX)) as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, bytes).map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
fn wait_for_audio_binding(
    timeout: std::time::Duration,
    mut resolve: impl FnMut() -> Result<hooviestar_engine::project::AudioSessionBinding, String>,
) -> Result<hooviestar_engine::project::AudioSessionBinding, String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_error = String::new();
    while std::time::Instant::now() < deadline {
        match resolve() {
            Ok(binding) => return Ok(binding),
            Err(error) => last_error = error,
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(format!(
        "audio session did not become uniquely available within {timeout:?}: {last_error}"
    ))
}

#[cfg(target_os = "windows")]
fn set_stage(
    engine: &hooviestar_engine::EngineHandle,
    scene_id: uuid::Uuid,
    browser_audio_id: uuid::Uuid,
    tone_audio_id: uuid::Uuid,
    browser_volume: f32,
    tone_muted: bool,
) -> Result<(), String> {
    use hooviestar_engine::EngineCommand;

    engine
        .command(EngineCommand::SetAudioVolume {
            source_id: browser_audio_id,
            volume: browser_volume,
        })
        .map_err(|error| error.to_string())?;
    engine
        .command(EngineCommand::SetAudioMuted {
            source_id: browser_audio_id,
            muted: browser_volume == 0.0,
        })
        .map_err(|error| error.to_string())?;
    engine
        .command(EngineCommand::SetAudioVolume {
            source_id: tone_audio_id,
            volume: if tone_muted {
                0.0
            } else if browser_volume > 0.0 {
                0.5
            } else {
                1.0
            },
        })
        .map_err(|error| error.to_string())?;
    engine
        .command(EngineCommand::SetAudioMuted {
            source_id: tone_audio_id,
            muted: tone_muted,
        })
        .map_err(|error| error.to_string())?;
    engine
        .command(EngineCommand::SetActiveScene { scene_id })
        .map_err(|error| error.to_string())
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("qualify_windows_pipeline requires Windows");
    std::process::exit(2);
}
