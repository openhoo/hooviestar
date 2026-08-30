#[path = "../examples/support/analysis.rs"]
mod analysis;

use analysis::{BgraFrame, Marker, analyze_signal, runtime_audio_process_id};

fn frame(width: u32, height: u32, color: [u8; 4]) -> BgraFrame {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for _ in 0..width * height {
        pixels.extend_from_slice(&color);
    }
    BgraFrame {
        width,
        height,
        pixels,
    }
}

#[test]
fn receiver_frequency_oracle_separates_browser_and_tone_bins() {
    let samples = (0..48_000)
        .map(|index| {
            let seconds = index as f64 / 48_000.0;
            0.2 * (std::f64::consts::TAU * 660.0 * seconds).sin()
                + 0.005 * (std::f64::consts::TAU * 440.0 * seconds).sin()
        })
        .collect::<Vec<_>>();
    let metrics = analyze_signal(&samples, &[660.0, 440.0]);
    assert!(metrics.amplitude(660.0) > 0.19);
    assert!(metrics.amplitude(660.0) > metrics.amplitude(440.0) * 30.0);
    assert!((metrics.rms - 0.1415).abs() < 0.002);
}

#[test]
fn white_browser_motion_cannot_impersonate_blue_muted_marker() {
    assert_eq!(frame(320, 180, [255, 255, 255, 255]).marker(), None);
    assert_eq!(
        frame(320, 180, [255, 0, 0, 255]).marker(),
        Some(Marker::Muted)
    );
}

#[test]
fn browser_fixture_palette_cannot_impersonate_stage_markers() {
    let fixture_bgra = [
        [0x44, 0x22, 0x11, 0xff],
        [0x44, 0x33, 0xcc, 0xff],
        [0x55, 0xaa, 0x33, 0xff],
        [0x1f, 0x43, 0x7b, 0xff],
    ];
    for color in fixture_bgra {
        assert_eq!(frame(320, 180, color).marker(), None);
    }
}

#[test]
fn marker_oracle_distinguishes_all_four_stages() {
    let cases = [
        ([255, 0, 255, 255], Marker::Browser),
        ([255, 255, 0, 255], Marker::Tone),
        ([0, 255, 255, 255], Marker::Mixed),
        ([255, 0, 0, 255], Marker::Muted),
    ];
    for (bgra, expected) in cases {
        assert_eq!(frame(320, 180, bgra).marker(), Some(expected));
    }
}

#[test]
fn motion_oracle_is_zero_for_identical_frames_and_detects_changed_pixels() {
    let first = frame(320, 180, [20, 40, 60, 255]);
    let mut changed = first.clone();
    for pixel in changed.pixels.as_chunks_mut::<4>().0.iter_mut().take(4_000) {
        *pixel = [200, 210, 220, 255];
    }
    assert_eq!(first.motion_ratio(&first), 0.0);
    assert!(first.motion_ratio(&changed) > 0.05);
}

#[test]
fn windows_audio_instance_identifier_yields_process_id() {
    let windows_11 = "{0.0.0.00000000}.{endpoint}|\\Device\\app.exe%b{group}|1%b9080";
    let legacy = "{0.0.0.00000000}.{endpoint}|\\Device\\app.exe%9080";
    assert_eq!(runtime_audio_process_id(windows_11), Some(9080));
    assert_eq!(runtime_audio_process_id(legacy), Some(9080));
    assert_eq!(runtime_audio_process_id("no-process-id"), None);
}
