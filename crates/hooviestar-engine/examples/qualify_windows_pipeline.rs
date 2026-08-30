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
    use std::{path::PathBuf, thread, time::Duration};

    use hooviestar_engine::{
        EngineCommand, EngineHandle, NativeSurfaceKind, NativeSurfaces, OutputConfig,
        audio::windows_runtime::ProcessAudioCapture,
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
        output_resize_kept_rendering: bool,
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
    let report_path = PathBuf::from(value("--report").unwrap_or("windows-pipeline-report.json"));

    let (browser_hwnd, browser_window_binding) =
        wait_for_window_binding(BROWSER_TITLE, Duration::from_secs(30))?;
    let browser_root = window_process_id(browser_hwnd);
    let browser_audio_binding = wait_for_audio_binding(Duration::from_secs(30), || {
        audio_binding_for_tree(&process_tree(browser_root)?)
    })?;
    let tone_audio_binding =
        wait_for_audio_binding(Duration::from_secs(30), || audio_binding_for_pid(tone_pid))?;

    let program = TestWindow::create(PROGRAM_TITLE, 1280, 720, true)?;
    let preview = TestWindow::create(PREVIEW_TITLE, 1280, 720, true)?;
    program.assert_discord_captureable_offscreen()?;

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
                binding: browser_audio_binding,
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
                binding: tone_audio_binding,
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
    thread::sleep(Duration::from_millis(1_200));
    let _ = program_capture.take_latest();
    let resized = wait_for_marker(
        &program_capture,
        &program_device,
        Marker::Mixed,
        Duration::from_secs(10),
    )?;
    engine
        .command(EngineCommand::SetOutputConfig {
            output: OutputConfig::default(),
        })
        .map_err(|error| error.to_string())?;

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
        program_mapped_offscreen: true,
        program_capture_width: browser_frame.width,
        program_capture_height: browser_frame.height,
        preview_capture_width: preview_frame.width,
        preview_capture_height: preview_frame.height,
        browser_motion_ratio,
        browser_scene_marker: browser_frame.marker() == Some(Marker::Browser),
        tone_scene_marker: tone_marker.marker() == Some(Marker::Tone),
        mixed_scene_marker: mixed_marker.marker() == Some(Marker::Mixed),
        muted_scene_marker: muted_marker.marker() == Some(Marker::Muted),
        output_resize_kept_rendering: resized.marker() == Some(Marker::Mixed),
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
        && visual.output_resize_kept_rendering
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

    if hold_seconds > 0 {
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
                browser_audio_id,
                tone_audio_id,
                browser_volume,
                tone_muted,
            )?;
            println!("{transport_label} stage: {:?}", stages[index % 4].1);
            thread::sleep(Duration::from_secs(8));
            index += 1;
        }
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
