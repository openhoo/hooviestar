#![allow(dead_code)]

use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    slice, thread,
    time::{Duration, Instant},
};

use hooviestar_engine::{
    SourceCandidate,
    audio::{SAMPLE_RATE, windows_runtime::ProcessAudioCapture},
    discovery::windows::{enumerate_audio_sessions, enumerate_visible_windows},
    project::{AudioSessionBinding, WindowBinding},
    video::windows::{D3d11Device, WindowCapture, WindowsVideoError},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, RECT},
        Graphics::{
            Direct3D11::{
                D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Texture2D,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
        },
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GetSystemMetrics, GetWindowRect,
            GetWindowThreadProcessId, HWND_NOTOPMOST, HWND_TOPMOST, IsIconic, IsWindowVisible,
            SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
            SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, WINDOW_EX_STYLE, WS_CLIPCHILDREN,
            WS_CLIPSIBLINGS, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
        },
    },
    core::{PCWSTR, w},
};

pub use super::analysis::{
    BROWSER_FREQUENCY_HZ, BgraFrame, Marker, MotionMetrics, SignalMetrics, TONE_FREQUENCY_HZ,
    analyze_signal, quiet_enough, runtime_audio_process_id, summarize_motion,
};

pub const BROWSER_TITLE: &str = "Hooviestar Browser Video Fixture";
pub const PROGRAM_TITLE: &str = "Hooviestar – Program";
pub const PREVIEW_TITLE: &str = "Hooviestar – Preview Qualification";

pub struct TestWindow {
    hwnd: HWND,
}

impl TestWindow {
    pub fn create(title: &str, width: i32, height: i32, offscreen: bool) -> Result<Self, String> {
        let virtual_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let virtual_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let x = if offscreen {
            virtual_left.saturating_sub(width).saturating_sub(128)
        } else {
            virtual_left
        };
        let y = if offscreen {
            virtual_top.saturating_add(64)
        } else {
            virtual_top
        };
        let extended_style = if offscreen {
            WINDOW_EX_STYLE::default()
        } else {
            WS_EX_TOPMOST
        };
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let hwnd = unsafe {
            CreateWindowExW(
                extended_style,
                w!("STATIC"),
                PCWSTR(title.as_ptr()),
                WS_POPUP | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                x,
                y,
                width,
                height,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|error| format!("test window {title:?} could not be created: {error}"))?;
        Ok(Self { hwnd })
    }

    pub fn raw(&self) -> usize {
        self.hwnd.0 as usize
    }

    pub fn raise_topmost(&self) -> Result<(), String> {
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            )
        }
        .map_err(|error| format!("Program could not be raised: {error}"))
    }

    pub fn release_topmost(&self) -> Result<(), String> {
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            )
        }
        .map_err(|error| format!("Program could not release topmost state: {error}"))
    }

    pub fn move_onscreen(&self) -> Result<(), String> {
        let virtual_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let virtual_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                virtual_left,
                virtual_top,
                0,
                0,
                SWP_NOSIZE | SWP_SHOWWINDOW,
            )
        }
        .map_err(|error| format!("Program could not move onscreen: {error}"))
    }

    pub fn assert_discord_captureable_offscreen(&self) -> Result<(), String> {
        if !unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
            return Err("Program window is not mapped/visible to capture APIs".into());
        }
        if unsafe { IsIconic(self.hwnd) }.as_bool() {
            return Err("Program window is minimized".into());
        }
        let mut window = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut window) }
            .map_err(|error| format!("Program rectangle unavailable: {error}"))?;
        let desktop = virtual_desktop_rect()?;
        if rectangles_intersect(window, desktop) {
            return Err(format!(
                "Program window intersects physical desktop: window={window:?}, desktop={desktop:?}"
            ));
        }
        Ok(())
    }

    pub fn assert_captureable_onscreen(&self) -> Result<(), String> {
        if !unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
            return Err("Program window is not mapped/visible to capture APIs".into());
        }
        if unsafe { IsIconic(self.hwnd) }.as_bool() {
            return Err("Program window is minimized".into());
        }
        let mut window = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut window) }
            .map_err(|error| format!("Program rectangle unavailable: {error}"))?;
        let desktop = virtual_desktop_rect()?;
        if !rectangles_intersect(window, desktop) {
            return Err(format!(
                "Program window does not intersect physical desktop: window={window:?}, desktop={desktop:?}"
            ));
        }
        Ok(())
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

fn virtual_desktop_rect() -> Result<RECT, String> {
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if width <= 0 || height <= 0 {
        return Err("Windows reports no virtual desktop".into());
    }
    Ok(RECT {
        left,
        top,
        right: left.saturating_add(width),
        bottom: top.saturating_add(height),
    })
}

fn rectangles_intersect(left: RECT, right: RECT) -> bool {
    left.left < right.right
        && left.right > right.left
        && left.top < right.bottom
        && left.bottom > right.top
}

pub fn readback_bgra8(
    device: &D3d11Device,
    texture: &ID3D11Texture2D,
) -> Result<BgraFrame, String> {
    let mut descriptor = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut descriptor) };
    if descriptor.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        return Err(format!(
            "capture texture has unexpected format {:?}",
            descriptor.Format
        ));
    }
    let staging_descriptor = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..descriptor
    };
    let mut staging = None;
    unsafe {
        device
            .device
            .CreateTexture2D(&staging_descriptor, None, Some(&mut staging))
    }
    .map_err(|error| format!("capture staging texture failed: {error}"))?;
    let staging = staging.ok_or_else(|| "D3D11 returned no staging texture".to_string())?;
    unsafe { device.immediate_context.CopyResource(&staging, texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        device
            .immediate_context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
    }
    .map_err(|error| format!("capture staging map failed: {error}"))?;
    let result = (|| {
        if mapped.pData.is_null() {
            return Err("D3D11 mapped a null capture buffer".to_string());
        }
        let row_bytes = descriptor.Width as usize * 4;
        let mut pixels = vec![0u8; row_bytes * descriptor.Height as usize];
        for row in 0..descriptor.Height as usize {
            let source = unsafe {
                slice::from_raw_parts(
                    mapped
                        .pData
                        .cast::<u8>()
                        .add(row * mapped.RowPitch as usize),
                    row_bytes,
                )
            };
            pixels[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(source);
        }
        Ok(BgraFrame {
            width: descriptor.Width,
            height: descriptor.Height,
            pixels,
        })
    })();
    unsafe { device.immediate_context.Unmap(&staging, 0) };
    result
}

pub fn wait_for_frame(
    capture: &WindowCapture,
    device: &D3d11Device,
    timeout: Duration,
) -> Result<BgraFrame, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(frame) = capture.take_latest() {
            return readback_bgra8(device, &frame.texture);
        }
        if capture.is_closed() {
            return Err("capture target closed".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(format!("capture delivered no frame within {timeout:?}"))
}

pub fn wait_for_marker(
    capture: &WindowCapture,
    device: &D3d11Device,
    expected: Marker,
    timeout: Duration,
) -> Result<BgraFrame, String> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    let mut last_frame = None;
    while Instant::now() < deadline {
        let frame = wait_for_frame(capture, device, Duration::from_secs(2))?;
        last = frame.marker();
        if last == Some(expected) {
            return Ok(frame);
        }
        last_frame = Some(frame);
    }
    if let (Some(frame), Some(path)) = (
        last_frame,
        std::env::var_os("HOOVIESTAR_QUALIFICATION_FAILURE_FRAME"),
    ) {
        let _ = frame.write_ppm(std::path::Path::new(&path));
    }
    Err(format!(
        "marker {expected:?} not observed within {timeout:?}; last={last:?}"
    ))
}

pub fn measure_marker_motion(
    capture: &WindowCapture,
    device: &D3d11Device,
    expected: Marker,
    frame_pairs: usize,
    interval: Duration,
) -> Result<(BgraFrame, MotionMetrics), String> {
    let mut previous = wait_for_marker(capture, device, expected, Duration::from_secs(10))?;
    let first = previous.clone();
    let mut ratios = Vec::with_capacity(frame_pairs);
    for _ in 0..frame_pairs {
        thread::sleep(interval);
        let next = wait_for_marker(capture, device, expected, Duration::from_secs(3))?;
        ratios.push(previous.motion_ratio(&next));
        previous = next;
    }
    Ok((first, summarize_motion(&ratios, 0.0002)))
}

pub fn measure_capture_cadence(capture: &WindowCapture, duration: Duration) -> f64 {
    let _ = capture.take_latest();
    let started = Instant::now();
    let mut frames = 0usize;
    while started.elapsed() < duration {
        if capture.take_latest().is_some() {
            frames += 1;
        } else {
            thread::sleep(Duration::from_millis(1));
        }
    }
    frames as f64 / started.elapsed().as_secs_f64()
}

pub fn marker_absent_for(
    capture: &WindowCapture,
    device: &D3d11Device,
    marker: Marker,
    duration: Duration,
) -> Result<bool, String> {
    let deadline = Instant::now() + duration;
    let mut observed_frames = 0usize;
    while Instant::now() < deadline {
        let frame = wait_for_frame(capture, device, Duration::from_secs(2))?;
        observed_frames += 1;
        if frame.marker() == Some(marker) {
            return Ok(false);
        }
    }
    Ok(observed_frames >= 3)
}

pub fn start_window_capture(
    hwnd: usize,
) -> Result<(D3d11Device, WindowCapture), WindowsVideoError> {
    let device = D3d11Device::create_hardware()?;
    let capture = WindowCapture::start(&device, hwnd)?;
    Ok((device, capture))
}

pub fn wait_for_window_binding(
    title: &str,
    timeout: Duration,
) -> Result<(usize, WindowBinding), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let matches: Vec<_> = enumerate_visible_windows(&[])?
            .into_iter()
            .filter_map(|candidate| match candidate {
                SourceCandidate::Window {
                    runtime_id,
                    binding,
                    ..
                } if binding.window_title == title => {
                    parse_hwnd(&runtime_id).map(|hwnd| (hwnd, binding))
                }
                _ => None,
            })
            .collect();
        match matches.as_slice() {
            [only] => return Ok(only.clone()),
            [] => thread::sleep(Duration::from_millis(250)),
            _ => return Err(format!("multiple windows have exact title {title:?}")),
        }
    }
    Err(format!(
        "window {title:?} did not appear within {timeout:?}"
    ))
}

pub fn find_window_containing(fragment: &str) -> Result<(String, usize), String> {
    let mut matches: Vec<_> = enumerate_visible_windows(&[])?
        .into_iter()
        .filter_map(|candidate| match candidate {
            SourceCandidate::Window {
                runtime_id, name, ..
            } if name
                .to_ascii_lowercase()
                .contains(&fragment.to_ascii_lowercase()) =>
            {
                parse_hwnd(&runtime_id).map(|hwnd| (name, hwnd))
            }
            _ => None,
        })
        .collect();
    if matches.len() != 1 {
        let names = matches.drain(..).map(|(name, _)| name).collect::<Vec<_>>();
        return Err(format!(
            "window title fragment {fragment:?} matched {} windows: {names:?}",
            names.len()
        ));
    }
    Ok(matches.remove(0))
}

pub fn parse_hwnd(runtime_id: &str) -> Option<usize> {
    runtime_id
        .strip_prefix("hwnd:")
        .and_then(|value| usize::from_str_radix(value, 16).ok())
}

pub fn window_process_id(hwnd: usize) -> u32 {
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut process_id)) };
    process_id
}

pub fn process_tree(root: u32) -> Result<HashSet<u32>, String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|error| error.to_string())?;
    let mut parents = HashMap::<u32, u32>::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    unsafe {
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    let mut tree = HashSet::from([root]);
    loop {
        let before = tree.len();
        for (process_id, parent_id) in &parents {
            if tree.contains(parent_id) {
                tree.insert(*process_id);
            }
        }
        if tree.len() == before {
            break;
        }
    }
    Ok(tree)
}

pub fn audio_binding_for_tree(processes: &HashSet<u32>) -> Result<AudioSessionBinding, String> {
    let mut matches = enumerate_audio_sessions()?
        .into_iter()
        .filter_map(|candidate| match candidate {
            SourceCandidate::ApplicationAudio {
                runtime_id,
                binding,
                ..
            } if runtime_audio_process_id(&runtime_id)
                .is_some_and(|pid| processes.contains(&pid)) =>
            {
                Some(binding)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    matches.dedup_by(|left, right| {
        left.process_path.eq_ignore_ascii_case(&right.process_path)
            && left.session_grouping_id == right.session_grouping_id
    });
    match matches.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err("no application-audio session belongs to requested process tree".into()),
        _ => Err(format!(
            "{} application-audio sessions belong to requested process tree",
            matches.len()
        )),
    }
}

pub fn audio_binding_for_pid(process_id: u32) -> Result<AudioSessionBinding, String> {
    audio_binding_for_tree(&HashSet::from([process_id]))
}

pub fn measure_process_audio(
    capture: &ProcessAudioCapture,
    duration: Duration,
    frequencies: &[f64],
) -> SignalMetrics {
    // Discard bounded history so every stage starts after its settled command.
    // Consume only frames the WASAPI producer actually supplied: Windows timer
    // wakeups are not guaranteed to occur at exact 10-ms boundaries, and
    // counting synthetic underrun zeros or dropping a fixed 480 frames per
    // wakeup changes the apparent tone frequency.
    capture.clear();
    let frames = (duration.as_secs_f64() * f64::from(SAMPLE_RATE)).round() as usize;
    let mut mono = Vec::with_capacity(frames);
    let deadline = std::time::Instant::now() + duration + Duration::from_secs(3);
    while mono.len() < frames {
        let mut received = false;
        while mono.len() < frames {
            let Some(frame) = capture.try_pop() else {
                break;
            };
            received = true;
            mono.push((f64::from(frame[0]) + f64::from(frame[1])) * 0.5);
        }
        if mono.len() == frames {
            break;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!(
                "process audio measurement timed out after {}/{} captured frames",
                mono.len(),
                frames
            );
            mono.clear();
            break;
        }
        if !received {
            thread::sleep(Duration::from_millis(1));
        }
    }
    analyze_signal(&mono, frequencies)
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StereoSignalMetrics {
    pub sample_count: usize,
    pub left: SignalMetrics,
    pub right: SignalMetrics,
}

pub fn measure_process_audio_stereo(
    capture: &ProcessAudioCapture,
    duration: Duration,
    frequencies: &[f64],
) -> StereoSignalMetrics {
    capture.clear();
    let frames = (duration.as_secs_f64() * f64::from(SAMPLE_RATE)).round() as usize;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let deadline = Instant::now() + duration + Duration::from_secs(3);
    while left.len() < frames {
        let mut received = false;
        while left.len() < frames {
            let Some(frame) = capture.try_pop() else {
                break;
            };
            received = true;
            left.push(f64::from(frame[0]));
            right.push(f64::from(frame[1]));
        }
        if left.len() == frames {
            break;
        }
        if Instant::now() >= deadline {
            eprintln!(
                "stereo process audio measurement timed out after {}/{} captured frames",
                left.len(),
                frames
            );
            left.clear();
            right.clear();
            break;
        }
        if !received {
            thread::sleep(Duration::from_millis(1));
        }
    }
    StereoSignalMetrics {
        sample_count: left.len(),
        left: analyze_signal(&left, frequencies),
        right: analyze_signal(&right, frequencies),
    }
}
