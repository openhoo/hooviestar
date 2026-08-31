#[path = "../examples/support/analysis.rs"]
mod analysis;

use analysis::{
    BgraFrame, Marker, analyze_signal, cadence_matches_fps, cadence_scaled_30_to_60,
    capture_cadence_healthy, gain_matches, quiet_enough, runtime_audio_process_id,
    summarize_motion,
};

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

fn marker_frame(marker: Marker) -> BgraFrame {
    let mut result = frame(640, 360, [24, 20, 16, 255]);
    let color = match marker {
        Marker::Browser => [255, 0, 255, 255],
        Marker::Tone => [255, 255, 0, 255],
        Marker::Mixed => [0, 255, 255, 255],
        Marker::Muted => [255, 0, 0, 255],
    };
    for y in 18..66 {
        for x in 12..192 {
            let offset = (y * result.width as usize + x) * 4;
            result.pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    result
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
fn output_cadence_oracle_requires_both_targets_and_real_scaling() {
    assert!(cadence_scaled_30_to_60(28.5, 57.0));
    assert!(!cadence_scaled_30_to_60(28.5, 31.0));
    assert!(!cadence_scaled_30_to_60(57.0, 57.0));
    assert!(!cadence_scaled_30_to_60(f64::NAN, 57.0));
}

#[test]
fn output_capture_oracle_accepts_vm_compositor_cap_but_not_stalls() {
    assert!(capture_cadence_healthy(28.0, 29.0));
    assert!(!capture_cadence_healthy(28.0, 3.0));
    assert!(!capture_cadence_healthy(f64::INFINITY, 29.0));
}

#[test]
fn output_profile_oracle_pins_each_supported_frame_rate() {
    assert!(cadence_matches_fps(29.0, 30));
    assert!(cadence_matches_fps(58.0, 60));
    assert!(!cadence_matches_fps(58.0, 30));
    assert!(!cadence_matches_fps(29.0, 60));
    assert!(!cadence_matches_fps(f64::NAN, 30));
    assert!(!cadence_matches_fps(30.0, 24));
}

#[test]
fn white_browser_motion_cannot_impersonate_blue_muted_marker() {
    assert_eq!(frame(320, 180, [255, 255, 255, 255]).marker(), None);
    assert_eq!(marker_frame(Marker::Muted).marker(), Some(Marker::Muted));
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
    for expected in [Marker::Browser, Marker::Tone, Marker::Mixed, Marker::Muted] {
        let frame = marker_frame(expected);
        let observation = frame.marker_observation().unwrap();
        assert_eq!(observation.marker, expected);
        assert!(observation.component_fraction > 0.02);
        assert!(observation.fill_ratio > 0.95);
    }
}

#[test]
fn solid_color_frame_cannot_impersonate_bounded_marker_panel() {
    for color in [
        [255, 0, 255, 255],
        [255, 255, 0, 255],
        [0, 255, 255, 255],
        [255, 0, 0, 255],
    ] {
        assert_eq!(frame(640, 360, color).marker(), None);
    }
}

#[test]
fn sparse_marker_colored_video_noise_cannot_impersonate_panel() {
    let mut noisy = frame(640, 360, [24, 20, 16, 255]);
    for index in (0..noisy.pixels.len()).step_by(4 * 97) {
        noisy.pixels[index..index + 4].copy_from_slice(&[255, 0, 255, 255]);
    }
    assert_eq!(noisy.marker(), None);
}

#[test]
fn tiny_marker_colored_rectangle_cannot_impersonate_panel() {
    let mut tiny = frame(640, 360, [24, 20, 16, 255]);
    for y in 8..14 {
        for x in 8..32 {
            let offset = (y * tiny.width as usize + x) * 4;
            tiny.pixels[offset..offset + 4].copy_from_slice(&[255, 0, 255, 255]);
        }
    }
    assert_eq!(tiny.marker(), None);
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
fn region_difference_oracle_is_bounded_and_position_sensitive() {
    let baseline = frame(320, 180, [24, 20, 16, 255]);
    let mut changed = baseline.clone();
    for y in 40..100 {
        for x in 120..220 {
            let offset = (y * changed.width as usize + x) * 4;
            changed.pixels[offset..offset + 4].copy_from_slice(&[220, 210, 200, 255]);
        }
    }
    assert_eq!(
        baseline.difference_ratio_in_rect(&changed, 0, 0, 100, 100, 24),
        0.0
    );
    assert!(baseline.difference_ratio_in_rect(&changed, 100, 20, 140, 100, 24) > 0.3);
    assert_eq!(
        baseline.difference_ratio_in_rect(&changed, 300, 0, 40, 40, 24),
        0.0
    );
}

#[test]
fn one_motion_spike_does_not_satisfy_sustained_motion() {
    let motion = summarize_motion(&[0.0, 0.0, 0.2, 0.0, 0.0, 0.0], 0.0002);
    assert_eq!(motion.maximum_motion_ratio, 0.2);
    assert!(!motion.sustained(5, 0.5, 0.0002, 3));
    assert_eq!(motion.longest_frozen_run, 3);
}

#[test]
fn repeated_motion_satisfies_sustained_oracle() {
    let motion = summarize_motion(&[0.01, 0.02, 0.0, 0.03, 0.04, 0.05], 0.0002);
    assert!(motion.sustained(5, 0.5, 0.0002, 3));
    assert_eq!(motion.moving_frame_pairs, 5);
    assert!((motion.median_motion_ratio - 0.025).abs() < 1.0e-9);
}

#[test]
fn audio_oracle_reports_length_dc_clipping_and_crest_factor() {
    let samples = [0.0, 1.0, -1.0, 0.5, -0.5];
    let metrics = analyze_signal(&samples, &[]);
    assert_eq!(metrics.sample_count, samples.len());
    assert!(metrics.dc_offset.abs() < 1.0e-12);
    assert!((metrics.clipped_sample_ratio - 0.4).abs() < 1.0e-12);
    assert!(metrics.crest_factor > 1.0);
}

#[test]
fn gain_oracle_rejects_ignored_or_excessive_attenuation() {
    assert!(gain_matches(0.1, 0.2, 0.5, 0.08));
    assert!(!gain_matches(0.2, 0.2, 0.5, 0.08));
    assert!(!gain_matches(0.02, 0.2, 0.5, 0.08));
    assert!(!gain_matches(0.0, 0.0, 0.5, 0.08));
}

#[test]
fn mute_oracle_has_relative_and_absolute_noise_ceilings() {
    let quiet = analyze_signal(&vec![0.0001; 48_000], &[]);
    let noisy = analyze_signal(&vec![0.02; 48_000], &[]);
    assert!(quiet_enough(&quiet, 0.2, 0.12, 0.005));
    assert!(!quiet_enough(&noisy, 100.0, 0.12, 0.005));
    assert!(!quiet_enough(&analyze_signal(&[], &[]), 0.2, 0.12, 0.005));
}

#[test]
fn windows_audio_instance_identifier_yields_process_id() {
    let windows_11 = "{0.0.0.00000000}.{endpoint}|\\Device\\app.exe%b{group}|1%b9080";
    let legacy = "{0.0.0.00000000}.{endpoint}|\\Device\\app.exe%9080";
    assert_eq!(runtime_audio_process_id(windows_11), Some(9080));
    assert_eq!(runtime_audio_process_id(legacy), Some(9080));
    assert_eq!(runtime_audio_process_id("no-process-id"), None);
}
