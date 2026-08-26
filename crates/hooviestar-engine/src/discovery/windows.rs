use crate::{
    audio::journal::{RestoreJournal, SessionRestoreEntry, quarantine_corrupt},
    discovery::SourceCandidate,
    project::{AudioSessionBinding, DisplayBinding, WindowBinding},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM},
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
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
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
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
    collect_visible_windows(excluded, true)
}

/// Shared collector behind the picker and resolver paths. Only the picker drops
/// ambiguous duplicates; resolve must see them so a genuinely ambiguous binding
/// reports "ambiguous" instead of the misleading "offline".
fn collect_visible_windows(
    excluded: &[usize],
    drop_ambiguous: bool,
) -> Result<Vec<SourceCandidate>, String> {
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
    if drop_ambiguous {
        filter_ambiguous_windows(&mut candidates);
    }
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
    // DWM-cloaked windows pass IsWindowVisible while being shown on no monitor
    // (suspended UWP hosts, hidden shell windows); a failing query simply means
    // the window exposes no cloak attribute and stays listed.
    let mut cloaked = 0u32;
    if unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok()
        && cloaked != 0
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
    let mut matches = collect_visible_windows(excluded, false)?
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
                let mut journal =
                    RestoreJournal::load(&journal_path).map_err(|error| error.to_string())?;
                // Insert-only baseline: repeated mutes of one session instance
                // must never overwrite the first recorded pre-mute state, or a
                // transient restore failure followed by recapture destroys the
                // user's original setting permanently.
                let existing = journal
                    .entries
                    .iter()
                    .find(|entry| entry.session_instance_id == instance_id)
                    .cloned();
                let entry = match existing {
                    Some(existing) => existing,
                    None => {
                        let original_mute = unsafe { volume.GetMute() }
                            .map_err(|error| error.to_string())?
                            .as_bool();
                        let entry = SessionRestoreEntry {
                            session_instance_id: instance_id,
                            process_path: process_path.into(),
                            original_mute,
                        };
                        journal.entries.push(entry.clone());
                        journal
                            .save_atomic(&journal_path)
                            .map_err(|error| error.to_string())?;
                        entry
                    }
                };
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

/// Prüft, ob wenigstens ein lebender Prozess denselben Imagedatei-Namen hat
/// wie der gespeicherte Pfad. Konservativ: Ohne vergleichbaren Namen oder bei
/// Snapshot-Fehler wird „vorhanden“ gemeldet, damit keine Wiederherstellungs-
/// baseline voreilig verworfen wird.
fn process_image_still_running(process_path: &std::path::Path) -> bool {
    let Some(image) = process_path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return true;
    };
    let mut entry = PROCESSENTRY32W::default();
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut found = false;
    unsafe {
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name_len = entry
                    .szExeFile
                    .iter()
                    .position(|character| *character == 0)
                    .unwrap_or(entry.szExeFile.len());
                let executable = String::from_utf16_lossy(&entry.szExeFile[..name_len]);
                if executable.eq_ignore_ascii_case(image) {
                    found = true;
                    break;
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    found
}

/// Instanz-IDs aller aktuell vorhandenen Anwendungssitzungen; `None`, wenn
/// die Enumeration fehlschlug — dann wird konservativ kein Eintrag
/// zusätzlich verworfen.
fn live_session_instance_ids() -> Option<std::collections::HashSet<String>> {
    initialize_com(|| {
        Ok(audio_session_records()?
            .into_iter()
            .filter_map(|(candidate, _)| match candidate {
                SourceCandidate::ApplicationAudio { runtime_id, .. } => Some(runtime_id),
                _ => None,
            })
            .collect())
    })
    .ok()
}

/// Lädt das Wiederherstellungsjournal, setzt noch stummgeschaltete Sitzungen
/// zurück und schreibt die bereinigten Einträge fort. Rückgabe
/// `Some((Fehler, Pfad))`, wenn das Journal unlesbar war oder sich nicht mehr
/// ersatzlos schreiben ließ — Pfad ist der Quarantäne-Ort bzw. das
/// Originaljournal, wenn Quarantäne oder Ersetzung selbst fehlschlugen (der
/// Fehler enthält dann beide Meldungen). Aufrufer können das sichtbar machen,
/// denn die darin liegenden `original_mute`-Baselines sind mit den Bytes
/// unwiederbringlich verloren und betroffene Sitzungen bleiben ggf. stumm.
/// Am unlesbaren Journal oder an einer fehlgeschlagenen Ersetzung scheitert
/// der Start nie.
pub fn repair_audio_journal(
    path: &std::path::Path,
) -> Result<Option<(String, std::path::PathBuf)>, String> {
    let mut quarantined = None;
    let mut journal = match RestoreJournal::load(path) {
        Ok(journal) => journal,
        // An unreadable journal (legacy schema, truncation, manual edit,
        // sharing violation because another process holds the file open)
        // must never take down the whole audio subsystem: quarantine the
        // file for diagnosis and continue with an empty journal, even when
        // the quarantine itself fails.
        Err(error) => {
            match quarantine_corrupt(path) {
                Ok(quarantined_path) => {
                    eprintln!(
                        "audio restore journal unreadable ({error}); quarantined as {}; \
                         starting with an empty journal",
                        quarantined_path.display()
                    );
                    quarantined = Some((error.to_string(), quarantined_path));
                }
                Err(io_error) => {
                    eprintln!(
                        "audio restore journal unreadable ({error}) and quarantine \
                         failed ({io_error}); starting with an empty journal",
                    );
                    quarantined = Some((
                        format!("{error}; quarantine failed: {io_error}"),
                        path.to_path_buf(),
                    ));
                }
            }
            RestoreJournal::default()
        }
    };
    let mut retained = Vec::new();
    // Instanz-Lookup nur einmal pro Durchlauf; `None` bewahrt alle Kandidaten.
    let mut live_instances: Option<std::collections::HashSet<String>> = None;
    let mut instances_queried = false;
    for entry in journal.entries.drain(..) {
        if restore_audio_session(&entry).is_ok() {
            continue;
        }
        // Gescheitert: Einträge eines beendeten Prozesses können nie mehr
        // gelingen (Instanz-IDs sind pro Sitzungserzeugung eindeutig) und
        // würden Journal und Startzeit grenzenlos wachsen lassen. Lebt der
        // Prozess noch, bleibt der Eintrag für den nächsten Versuch liegen —
        // es sei denn, die gespeicherte Instanz-ID existiert nicht mehr: Der
        // Image-Name überlebt auch einen Neustart der App, die alte
        // Instanz-ID nie; ein solcher Eintrag ist unerreichbar und wird
        // fallengelassen. Schlägt die Enumeration fehl, bleibt er liegen.
        if !process_image_still_running(&entry.process_path) {
            eprintln!(
                "dropping stale audio restore entry for exited process {}",
                entry.process_path.display()
            );
            continue;
        }
        if !instances_queried {
            live_instances = live_session_instance_ids();
            instances_queried = true;
        }
        match &live_instances {
            Some(live) if !live.contains(&entry.session_instance_id) => {
                eprintln!(
                    "dropping stale audio restore entry {}: session instance no \
                     longer exists (process {} likely restarted)",
                    entry.session_instance_id,
                    entry.process_path.display()
                );
                continue;
            }
            _ => {}
        }
        retained.push(entry);
    }
    journal.entries = retained;
    // Die Ersetzung des Journals darf den Start nie abbrechen: Ein lesbares
    // Journal, das sich nicht schreiben lässt (read-only Attribut,
    // restriktive ACL, Sharing-Verstoß eines anderen Prozesses), wird mit
    // dem reparierten In-Memory-Zustand weitergeführt und wie eine Quarantäne
    // sichtbar gemacht. Nur die Baselines der verbleibenden Einträge gehen
    // verloren.
    if let Err(error) = journal.save_atomic(path) {
        eprintln!(
            "audio restore journal could not be replaced ({error}); \
             continuing with the repaired in-memory journal"
        );
        return match quarantined.take() {
            Some((existing, quarantined_path)) => Ok(Some((
                format!("{existing}; journal replacement failed: {error}"),
                quarantined_path,
            ))),
            None => Ok(Some((
                format!("journal replacement failed: {error}"),
                path.to_path_buf(),
            ))),
        };
    }
    Ok(quarantined)
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
