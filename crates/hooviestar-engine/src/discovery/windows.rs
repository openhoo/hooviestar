use crate::{
    audio::journal::{RestoreJournal, SessionRestoreEntry},
    discovery::SourceCandidate,
    project::{AudioSessionBinding, DisplayBinding, WindowBinding},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM},
        Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIAdapter, IDXGIFactory1},
        Media::Audio::{
            IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
            MMDeviceEnumerator, eConsole, eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize,
            },
            Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
        },
    },
    core::{BOOL, Interface, PWSTR},
};

struct WindowEnumeration<'a> {
    excluded: &'a [usize],
    candidates: &'a mut Vec<SourceCandidate>,
}

pub fn enumerate_visible_windows(excluded: &[usize]) -> Result<Vec<SourceCandidate>, String> {
    let mut candidates = Vec::new();
    let mut context = WindowEnumeration {
        excluded,
        candidates: &mut candidates,
    };
    unsafe {
        EnumWindows(
            Some(enumerate_window),
            LPARAM((&mut context as *mut WindowEnumeration).cast::<()>() as isize),
        )
    }
    .map_err(|error| error.to_string())?;
    filter_ambiguous_windows(&mut candidates);
    candidates.sort_by(|left, right| candidate_name(left).cmp(candidate_name(right)));
    Ok(candidates)
}

unsafe extern "system" fn enumerate_window(window: HWND, parameter: LPARAM) -> BOOL {
    let context = unsafe { &mut *(parameter.0 as *mut WindowEnumeration<'_>) };
    if !unsafe { IsWindowVisible(window) }.as_bool()
        || context.excluded.contains(&(window.0 as usize))
    {
        return true.into();
    }
    let mut title = [0u16; 1024];
    let title_length = unsafe { GetWindowTextW(window, &mut title) };
    if title_length <= 0 {
        return true.into();
    }
    let title = String::from_utf16_lossy(&title[..title_length as usize]);
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    let Some(process_path) = process_path(process_id) else {
        return true.into();
    };
    context.candidates.push(SourceCandidate::Window {
        runtime_id: format!("hwnd:{:x}", window.0 as usize),
        name: title.clone(),
        binding: WindowBinding {
            process_path,
            window_title: title,
        },
    });
    true.into()
}

pub fn resolve_window(binding: &WindowBinding, excluded: &[usize]) -> Result<usize, String> {
    let mut matches = enumerate_visible_windows(excluded)?
        .into_iter()
        .filter_map(|candidate| match candidate {
            SourceCandidate::Window {
                runtime_id,
                binding: candidate,
                ..
            } if candidate
                .process_path
                .eq_ignore_ascii_case(&binding.process_path)
                && candidate.window_title == binding.window_title =>
            {
                runtime_id
                    .strip_prefix("hwnd:")
                    .and_then(|value| usize::from_str_radix(value, 16).ok())
            }
            _ => None,
        });
    let window = matches
        .next()
        .ok_or_else(|| "window source is offline".to_string())?;
    if matches.next().is_some() {
        return Err("window source binding is ambiguous".into());
    }
    Ok(window)
}

fn process_path(process_id: u32) -> Option<String> {
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    result.ok()?;
    Some(String::from_utf16_lossy(&buffer[..length as usize]))
}

pub fn enumerate_displays() -> Result<Vec<SourceCandidate>, String> {
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|error| error.to_string())?;
    let mut candidates = Vec::new();
    let mut adapter_index = 0u32;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(error.to_string()),
        };
        let description = unsafe { adapter.GetDesc1() }.map_err(|error| error.to_string())?;
        let luid = format!(
            "{:08x}:{:08x}",
            description.AdapterLuid.HighPart as u32, description.AdapterLuid.LowPart
        );
        let adapter_base: IDXGIAdapter = adapter.cast().map_err(|error| error.to_string())?;
        let mut output_index = 0u32;
        loop {
            let output = match unsafe { adapter_base.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(error.to_string()),
            };
            let output_description =
                unsafe { output.GetDesc() }.map_err(|error| error.to_string())?;
            let end = output_description
                .DeviceName
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(output_description.DeviceName.len());
            let name = String::from_utf16_lossy(&output_description.DeviceName[..end]);
            candidates.push(SourceCandidate::Display {
                runtime_id: format!("display:{adapter_index}:{output_index}"),
                name,
                binding: DisplayBinding {
                    adapter_luid: luid.clone(),
                    output_id: output_index,
                },
            });
            output_index += 1;
        }
        adapter_index += 1;
    }
    Ok(candidates)
}

pub fn enumerate_audio_sessions() -> Result<Vec<SourceCandidate>, String> {
    std::thread::Builder::new()
        .name("audio-session-enumeration".into())
        .spawn(|| {
            initialize_com(|| {
                audio_session_records().map(|records| {
                    records
                        .into_iter()
                        .map(|(candidate, _)| candidate)
                        .collect()
                })
            })
        })
        .map_err(|error| error.to_string())?
        .join()
        .map_err(|_| "audio session enumeration panicked".to_string())?
}

pub fn resolve_audio_process(binding: &AudioSessionBinding) -> Result<u32, String> {
    let binding = binding.clone();
    std::thread::Builder::new()
        .name("audio-session-resolution".into())
        .spawn(move || {
            initialize_com(|| {
                let mut matches =
                    audio_session_records()?
                        .into_iter()
                        .filter_map(|(candidate, process_id)| match candidate {
                            SourceCandidate::ApplicationAudio {
                                binding: candidate, ..
                            } if candidate
                                .process_path
                                .eq_ignore_ascii_case(&binding.process_path)
                                && candidate.session_grouping_id == binding.session_grouping_id =>
                            {
                                Some(process_id)
                            }
                            _ => None,
                        });
                let process_id = matches
                    .next()
                    .ok_or_else(|| "application audio session is offline".to_string())?;
                if matches.next().is_some() {
                    return Err("application audio session binding is ambiguous".into());
                }
                Ok(process_id)
            })
        })
        .map_err(|error| error.to_string())?
        .join()
        .map_err(|_| "audio session resolution panicked".to_string())?
}

fn initialize_com<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|error| error.to_string())?;
    let result = operation();
    unsafe { CoUninitialize() };
    result
}

fn audio_session_records() -> Result<Vec<(SourceCandidate, u32)>, String> {
    let device_enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|error| error.to_string())?;
    let endpoint = unsafe { device_enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|error| error.to_string())?;
    let manager: IAudioSessionManager2 =
        unsafe { endpoint.Activate(CLSCTX_ALL, None) }.map_err(|error| error.to_string())?;
    let sessions = unsafe { manager.GetSessionEnumerator() }.map_err(|error| error.to_string())?;
    let count = unsafe { sessions.GetCount() }.map_err(|error| error.to_string())?;
    let mut candidates = Vec::new();
    for index in 0..count {
        let control = unsafe { sessions.GetSession(index) }.map_err(|error| error.to_string())?;
        let control2: IAudioSessionControl2 = control.cast().map_err(|error| error.to_string())?;
        let process_id = unsafe { control2.GetProcessId() }.map_err(|error| error.to_string())?;
        if process_id == 0 || process_id == std::process::id() {
            continue;
        }
        let Some(process_path) = process_path(process_id) else {
            continue;
        };
        let grouping = format!(
            "{:?}",
            unsafe { control.GetGroupingParam() }.map_err(|error| error.to_string())?
        );
        let runtime_id_value = unsafe { control2.GetSessionInstanceIdentifier() }
            .map_err(|error| error.to_string())?;
        let runtime_id = pwstr_string(runtime_id_value)?;
        let display_name_value =
            unsafe { control.GetDisplayName() }.map_err(|error| error.to_string())?;
        let display_name = pwstr_string(display_name_value).unwrap_or_default();
        let name = if display_name.trim().is_empty() {
            std::path::Path::new(&process_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&process_path)
                .to_string()
        } else {
            display_name
        };
        candidates.push((
            SourceCandidate::ApplicationAudio {
                runtime_id,
                name,
                binding: AudioSessionBinding {
                    process_path,
                    session_grouping_id: grouping,
                },
            },
            process_id,
        ));
    }
    candidates.sort_by(|left, right| candidate_name(&left.0).cmp(candidate_name(&right.0)));
    Ok(candidates)
}

pub fn mute_audio_session(
    binding: &AudioSessionBinding,
    journal_path: &std::path::Path,
) -> Result<SessionRestoreEntry, String> {
    let binding = binding.clone();
    let journal_path = journal_path.to_path_buf();
    std::thread::Builder::new()
        .name("audio-session-mute".into())
        .spawn(move || {
            initialize_com(|| {
                let (instance_id, process_path, volume) = find_audio_volume(&binding)?;
                let original_mute = unsafe { volume.GetMute() }
                    .map_err(|error| error.to_string())?
                    .as_bool();
                let entry = SessionRestoreEntry {
                    session_instance_id: instance_id,
                    process_path: process_path.into(),
                    original_mute,
                };
                let mut journal =
                    RestoreJournal::load(&journal_path).map_err(|error| error.to_string())?;
                journal.upsert(entry.clone());
                journal
                    .save_atomic(&journal_path)
                    .map_err(|error| error.to_string())?;
                let context = windows::core::GUID::zeroed();
                unsafe { volume.SetMute(true, &context) }.map_err(|error| error.to_string())?;
                Ok(entry)
            })
        })
        .map_err(|error| error.to_string())?
        .join()
        .map_err(|_| "audio session mute panicked".to_string())?
}

pub fn restore_audio_session(entry: &SessionRestoreEntry) -> Result<(), String> {
    let entry = entry.clone();
    std::thread::Builder::new()
        .name("audio-session-restore".into())
        .spawn(move || {
            initialize_com(|| {
                let (_, _, volume) = find_audio_volume_by_instance(
                    &entry.session_instance_id,
                    &entry.process_path.to_string_lossy(),
                )?;
                let context = windows::core::GUID::zeroed();
                unsafe { volume.SetMute(entry.original_mute, &context) }
                    .map_err(|error| error.to_string())
            })
        })
        .map_err(|error| error.to_string())?
        .join()
        .map_err(|_| "audio session restore panicked".to_string())?
}

pub fn repair_audio_journal(path: &std::path::Path) -> Result<(), String> {
    let mut journal = RestoreJournal::load(path).map_err(|error| error.to_string())?;
    let mut retained = Vec::new();
    for entry in journal.entries.drain(..) {
        if restore_audio_session(&entry).is_err() {
            retained.push(entry);
        }
    }
    journal.entries = retained;
    journal.save_atomic(path).map_err(|error| error.to_string())
}

fn find_audio_volume(
    binding: &AudioSessionBinding,
) -> Result<(String, String, ISimpleAudioVolume), String> {
    find_audio_volume_matching(|path, grouping, _| {
        path.eq_ignore_ascii_case(&binding.process_path) && grouping == binding.session_grouping_id
    })
}

fn find_audio_volume_by_instance(
    instance_id: &str,
    process_path: &str,
) -> Result<(String, String, ISimpleAudioVolume), String> {
    find_audio_volume_matching(|path, _, instance| {
        path.eq_ignore_ascii_case(process_path) && instance == instance_id
    })
}

fn find_audio_volume_matching(
    predicate: impl Fn(&str, &str, &str) -> bool,
) -> Result<(String, String, ISimpleAudioVolume), String> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|error| error.to_string())?;
    let endpoint = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|error| error.to_string())?;
    let manager: IAudioSessionManager2 =
        unsafe { endpoint.Activate(CLSCTX_ALL, None) }.map_err(|error| error.to_string())?;
    let sessions = unsafe { manager.GetSessionEnumerator() }.map_err(|error| error.to_string())?;
    let count = unsafe { sessions.GetCount() }.map_err(|error| error.to_string())?;
    let mut matches = Vec::new();
    for index in 0..count {
        let control = unsafe { sessions.GetSession(index) }.map_err(|error| error.to_string())?;
        let control2: IAudioSessionControl2 = control.cast().map_err(|error| error.to_string())?;
        let process_id = unsafe { control2.GetProcessId() }.map_err(|error| error.to_string())?;
        if process_id == std::process::id() {
            continue;
        }
        let Some(path) = process_path(process_id) else {
            continue;
        };
        let grouping = format!(
            "{:?}",
            unsafe { control.GetGroupingParam() }.map_err(|error| error.to_string())?
        );
        let instance_value = unsafe { control2.GetSessionInstanceIdentifier() }
            .map_err(|error| error.to_string())?;
        let instance = pwstr_string(instance_value)?;
        if predicate(&path, &grouping, &instance) {
            let volume: ISimpleAudioVolume = control.cast().map_err(|error| error.to_string())?;
            matches.push((instance, path, volume));
        }
    }
    if matches.len() != 1 {
        return Err(if matches.is_empty() {
            "application audio session is offline".into()
        } else {
            "application audio session binding is ambiguous".into()
        });
    }
    Ok(matches.remove(0))
}

fn pwstr_string(value: PWSTR) -> Result<String, String> {
    if value.is_null() {
        return Ok(String::new());
    }
    let result = unsafe { value.to_string() }.map_err(|error| error.to_string());
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    result
}

fn candidate_name(candidate: &SourceCandidate) -> &str {
    match candidate {
        SourceCandidate::Window { name, .. }
        | SourceCandidate::Display { name, .. }
        | SourceCandidate::ApplicationAudio { name, .. } => name,
    }
}

fn filter_ambiguous_windows(candidates: &mut Vec<SourceCandidate>) {
    let mut counts = std::collections::HashMap::<(String, String), usize>::new();
    for candidate in candidates.iter() {
        if let SourceCandidate::Window { binding, .. } = candidate {
            *counts.entry(window_binding_key(binding)).or_default() += 1;
        }
    }
    candidates.retain(|candidate| match candidate {
        SourceCandidate::Window { binding, .. } => counts
            .get(&window_binding_key(binding))
            .is_some_and(|count| *count == 1),
        _ => true,
    });
}

fn window_binding_key(binding: &WindowBinding) -> (String, String) {
    (
        binding.process_path.to_ascii_lowercase(),
        binding.window_title.clone(),
    )
}
