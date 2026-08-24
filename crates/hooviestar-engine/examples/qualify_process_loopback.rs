#[cfg(target_os = "windows")]
fn main() {
    use std::{thread, time::Duration};

    use hooviestar_engine::{
        audio::windows_runtime::ProcessAudioCapture,
        discovery::{
            SourceCandidate,
            windows::{enumerate_audio_sessions, mute_audio_session, restore_audio_session},
        },
    };

    let candidate = enumerate_audio_sessions()
        .expect("session enumeration failed")
        .into_iter()
        .find_map(|candidate| match candidate {
            SourceCandidate::ApplicationAudio {
                runtime_id,
                binding,
                ..
            } if binding
                .process_path
                .to_ascii_lowercase()
                .ends_with("tone_session.exe") =>
            {
                Some((runtime_id, binding))
            }
            _ => None,
        })
        .expect("tone session not found");
    let process_id = candidate
        .0
        .split('%')
        .find_map(|part| part.parse::<u32>().ok())
        .or_else(|| hooviestar_engine::discovery::windows::resolve_audio_process(&candidate.1).ok())
        .expect("tone process resolution failed");
    let capture = ProcessAudioCapture::start(process_id).expect("process loopback failed");
    thread::sleep(Duration::from_secs(2));
    let (before_rms, before_crossings) = measure(&capture);
    let journal = std::env::temp_dir().join("hooviestar-audio-qualification.json");
    let restore = mute_audio_session(&candidate.1, &journal).expect("session mute failed");
    thread::sleep(Duration::from_secs(3));
    let (muted_rms, muted_crossings) = measure(&capture);
    capture.shutdown();
    restore_audio_session(&restore).expect("session restore failed");
    let _ = std::fs::remove_file(journal);
    assert!(
        before_rms > 0.02,
        "unmuted process loopback RMS too low: {before_rms}"
    );
    assert!(
        before_crossings > 600,
        "unmuted tone crossings too low: {before_crossings}"
    );
    println!(
        "process loopback ready: unmuted_rms={before_rms:.4} unmuted_crossings={before_crossings} muted_rms={muted_rms:.4} muted_crossings={muted_crossings}"
    );
}

#[cfg(target_os = "windows")]
fn measure(capture: &hooviestar_engine::audio::windows_runtime::ProcessAudioCapture) -> (f64, u32) {
    let mut squares = 0.0f64;
    let mut crossings = 0u32;
    let mut previous = 0.0f32;
    for _ in 0..48_000 {
        let sample = capture.pop()[0];
        squares += f64::from(sample * sample);
        if (previous <= 0.0 && sample > 0.0) || (previous >= 0.0 && sample < 0.0) {
            crossings += 1;
        }
        previous = sample;
    }
    ((squares / 48_000.0).sqrt(), crossings)
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("qualify_process_loopback requires Windows");
    std::process::exit(2);
}
