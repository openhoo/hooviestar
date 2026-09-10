mod platform;
mod taskbar;
mod updater;

#[cfg(target_os = "windows")]
use std::sync::atomic::AtomicU64;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use hooviestar_engine::{
    EngineCommand, EngineEvent, EngineHandle, NativeSurfaces, OutputConfig, ProjectV1,
    SourceEnumeration,
};
use platform::NativePreview;
#[cfg(target_os = "linux")]
use tauri::WindowEvent;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, window::WindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_window_state::StateFlags;
use uuid::Uuid;

struct AppState {
    engine: Arc<EngineHandle>,
    preview: usize,
    surfaces: NativeSurfaces,
    hotkey_mutations: Mutex<()>,
    // Beim Start verpasste Hotkey-Fehler; das Webview hängt seinen Listener erst
    // nach get_snapshot an, darum emittiert engine_status sie beim Be-
    // reitschaftssignal direkt über den normalen Event-Pfad.
    initial_hotkey_failures: Mutex<Vec<(Uuid, String)>>,
    // Beim Start bereits in den Event-Kanal gestellte Engine-Ereignisse
    // (DeviceRecovery::Failed, EngineError); der Forwarder-Thread und der
    // Webview-Listener existieren noch nicht, darum emittiert engine_status
    // sie beim Bereitschaftssignal über den normalen Event-Pfad.
    initial_events: Mutex<Vec<EngineEvent>>,
    // Bereitschaftssignal des Webviews: Läuft engine_status noch nicht,
    // puffert der Forwarder-Thread Ereignisse hier (Cap 512, älteste werden
    // verworfen), statt sie an noch nicht registrierte Listener zu verlieren.
    events_ready: AtomicBool,
    pending_events: Mutex<Vec<EngineEvent>>,
    #[cfg(target_os = "windows")]
    picker_stage_generation: Arc<AtomicU64>,
    #[cfg(target_os = "windows")]
    picker_stage_active: Arc<AtomicBool>,
}

static STUDIO_ACTIVATION_RETRYING: AtomicBool = AtomicBool::new(false);

struct RuntimeResources {
    engine: Mutex<Option<Arc<EngineHandle>>>,
    preview: Mutex<Option<NativePreview>>,
    output_visibility: Mutex<Option<platform::OutputVisibility>>,
    event_thread: Mutex<Option<JoinHandle<()>>>,
    stop_events: Arc<AtomicBool>,
    cleaned: AtomicBool,
    portal: platform::PortalResources,
}

impl RuntimeResources {
    fn new() -> Self {
        Self {
            engine: Mutex::new(None),
            preview: Mutex::new(None),
            output_visibility: Mutex::new(None),
            event_thread: Mutex::new(None),
            stop_events: Arc::new(AtomicBool::new(false)),
            cleaned: AtomicBool::new(false),
            portal: platform::PortalResources::new(),
        }
    }

    fn cleanup(&self, app: &AppHandle) {
        if self.cleaned.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = app.global_shortcut().unregister_all();
        self.stop_events.store(true, Ordering::Release);
        let engine = self.engine.lock().expect("engine mutex poisoned").take();
        if let Some(engine) = engine
            && let Err(error) = engine.shutdown()
        {
            eprintln!("[hooviestar] engine shutdown persistence error: {error}");
        }
        let event_thread = self
            .event_thread
            .lock()
            .expect("event thread mutex poisoned")
            .take();
        if let Some(thread) = event_thread {
            let _ = thread.join();
        }
        let preview = self.preview.lock().expect("preview mutex poisoned").take();
        if let Some(preview) = preview {
            let _ = preview.destroy();
        }
        let visibility = self
            .output_visibility
            .lock()
            .expect("output visibility mutex poisoned")
            .take();
        if let Some(visibility) = visibility {
            visibility.cleanup();
        }
        self.portal.clear();
    }
}

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> ProjectV1 {
    state.inner().engine.snapshot()
}

#[tauri::command]
fn dispatch(
    command: EngineCommand,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if let EngineCommand::SetSceneHotkey { scene_id, hotkey } = &command {
        return update_scene_hotkey(
            &app,
            state.inner().engine.clone(),
            &state.inner().hotkey_mutations,
            *scene_id,
            hotkey.clone(),
        );
    }
    if let EngineCommand::RemoveScene { scene_id } = &command {
        let scene_id = *scene_id;
        let _hotkey_guard = state
            .inner()
            .hotkey_mutations
            .lock()
            .expect("hotkey mutex poisoned");
        let shortcut = state
            .inner()
            .engine
            .snapshot()
            .scenes
            .into_iter()
            .find(|scene| scene.id == scene_id)
            .and_then(|scene| scene.hotkey);
        // Registrierung zuerst entfernen: Scheitert das, bleibt die Szene
        // unverändert statt als unsichtbare Ghost-Bindung weiterzuleben.
        let unregistered = if let Some(shortcut) = &shortcut
            && app.global_shortcut().is_registered(shortcut.as_str())
        {
            app.global_shortcut()
                .unregister(shortcut.as_str())
                .map_err(|error| {
                    format!("Hotkey {shortcut} konnte nicht entfernt werden: {error}")
                })?;
            true
        } else {
            false
        };
        if let Err(error) = state.inner().engine.command(command) {
            if unregistered
                && let Some(shortcut) = &shortcut
                && let Err(rollback) =
                    register_hotkey(&app, state.inner().engine.clone(), scene_id, shortcut)
            {
                return Err(format!(
                    "{error}; Hotkey-Rollback für {shortcut} fehlgeschlagen: {rollback}"
                ));
            }
            return Err(error.to_string());
        }
        return Ok(());
    }
    state
        .inner()
        .engine
        .command(command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn engine_status(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<&'static str, String> {
    let mut events: Vec<EngineEvent> = state
        .inner()
        .initial_events
        .lock()
        .expect("startup event mutex poisoned")
        .drain(..)
        .collect();
    let pending: Vec<(Uuid, String)> = state
        .inner()
        .initial_hotkey_failures
        .lock()
        .expect("hotkey mutex poisoned")
        .drain(..)
        .collect();
    for (scene_id, message) in pending {
        events.push(EngineEvent::HotkeyError { scene_id, message });
    }
    // pending_events bleibt bis zum Ende des Replays gesperrt. Der Forwarder
    // sperrt denselben Gate-Mutex vor seinem Ready-Check; neuere Ereignisse
    // können die Startsequenz damit nicht überholen.
    let mut pending_guard = state
        .inner()
        .pending_events
        .lock()
        .expect("pending event mutex poisoned");
    state.events_ready.store(true, Ordering::Release);
    events.extend(pending_guard.drain(..));
    // Der Webview haengt seinen Listener vor diesem Aufruf an (engineStore
    // start(): listen() vor engine_status), darum gehen hier verpasste
    // Ereignisse direkt an den selben Emit-Pfad wie der Forwarder-Thread.
    for (index, event) in events.iter().enumerate() {
        if let Err(error) = app.emit("engine-event", event) {
            eprintln!("[hooviestar] failed to emit replayed engine event: {error}");
            *pending_guard = events[index..].to_vec();
            state.events_ready.store(false, Ordering::Release);
            // IPC-Fehler an den Webview zurückgeben: engineStore entfernt den
            // Listener und darf den vollständigen Handshake erneut versuchen.
            return Err(format!(
                "Engine-Ereignis konnte nicht zugestellt werden: {error}"
            ));
        }
    }
    Ok("running")
}

#[tauri::command]
async fn enumerate_sources(state: State<'_, AppState>) -> Result<SourceEnumeration, String> {
    platform::enumerate_sources(state.inner().surfaces).await
}

#[tauri::command]
async fn select_portal_sources(
    resources: State<'_, Arc<RuntimeResources>>,
) -> Result<SourceEnumeration, String> {
    resources.inner().portal.select().await
}

#[tauri::command]
async fn canonicalize_file(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let canonical = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
        if !canonical.is_file() {
            return Err("Ausgewählter Pfad ist keine reguläre Datei".into());
        }
        canonical
            .into_os_string()
            .into_string()
            .map_err(|_| "Ausgewählter Pfad ist kein gültiger Unicode-Pfad".to_string())
    })
    .await
    .map_err(|error| format!("Pfadprüfung fehlgeschlagen: {error}"))?
}

fn physical_preview_bounds(
    scale: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(i32, i32, i32, i32), String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err("Ungültiger Skalierungsfaktor für Vorschau".into());
    }
    if ![x, y, width, height].iter().all(|value| value.is_finite()) || width <= 0.0 || height <= 0.0
    {
        return Err("Ungültige Vorschaugeometrie".into());
    }
    let physical = |value: f64| {
        let scaled = (value * scale).round();
        if !scaled.is_finite() || scaled < i32::MIN as f64 || scaled > i32::MAX as f64 {
            Err("Vorschaugeometrie liegt außerhalb des unterstützten Bereichs".to_string())
        } else {
            Ok(scaled as i32)
        }
    };
    let bounds = (
        physical(x)?,
        physical(y)?,
        physical(width)?,
        physical(height)?,
    );
    if bounds.2 <= 0 || bounds.3 <= 0 {
        return Err("Vorschaugröße muss mindestens einen physischen Pixel betragen".into());
    }
    Ok(bounds)
}

#[tauri::command]
fn set_preview_bounds(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let studio = app
        .get_webview_window("studio")
        .ok_or_else(|| "Studio-Fenster nicht verfügbar".to_string())?;
    let scale = studio.scale_factor().map_err(|error| error.to_string())?;
    let (x, y, width, height) = physical_preview_bounds(scale, x, y, width, height)?;
    platform::set_preview_bounds(state.inner().preview, x, y, width, height)
}

#[tauri::command]
fn set_preview_overlay(
    visible: bool,
    output_width: f64,
    output_height: f64,
    selection: Option<platform::PreviewOverlaySelection>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let payload = platform::PreviewOverlayPayload {
        visible,
        output_width,
        output_height,
        selection,
    };
    payload.validate()?;
    platform::set_preview_overlay(state.inner().preview, payload)
}

#[tauri::command]
fn set_preview_visible(visible: bool, state: State<'_, AppState>) -> Result<(), String> {
    platform::set_preview_visible(state.inner().preview, visible)
}

#[cfg(target_os = "windows")]
fn conceal_program_after_picker(program: &tauri::Window, surface: usize) -> Result<(), String> {
    if let Err(error) = platform::restore_program_offscreen(surface) {
        let hide_error = program.hide().err();
        return Err(match hide_error {
            Some(hide_error) => format!(
                "Program konnte weder offscreen noch verborgen werden: {error}; {hide_error}"
            ),
            None => format!(
                "Program konnte nicht offscreen wiederhergestellt werden und wurde verborgen: {error}"
            ),
        });
    }
    let taskbar_error = program.set_skip_taskbar(true).err();
    let focusable_error = program.set_focusable(false).err();
    if taskbar_error.is_some() || focusable_error.is_some() {
        let hide_error = program.hide().err();
        let detail = [
            taskbar_error.map(|error| format!("Taskleiste: {error}")),
            focusable_error.map(|error| format!("Fokus: {error}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ");
        return Err(match hide_error {
            Some(hide_error) => format!(
                "Program-Nachbereitung scheiterte und Verbergen scheiterte: {detail}; {hide_error}"
            ),
            None => format!("Program-Nachbereitung scheiterte; Program wurde verborgen: {detail}"),
        });
    }
    Ok(())
}

#[tauri::command]
fn prepare_stream_picker(app: AppHandle, state: State<'_, AppState>) -> Result<u64, String> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, state);
        Err("Sichere Fensterauswahl ist derzeit nur unter Windows erforderlich".into())
    }

    #[cfg(target_os = "windows")]
    {
        const PICKER_SECONDS: u64 = 60;
        let studio = app
            .get_webview_window("studio")
            .ok_or_else(|| "Studio-Fenster nicht verfügbar".to_string())?;
        let program = app
            .get_window("program")
            .ok_or_else(|| "Program-Fenster nicht verfügbar".to_string())?;

        studio.show().map_err(|error| error.to_string())?;
        studio.unminimize().map_err(|error| error.to_string())?;
        studio.maximize().map_err(|error| error.to_string())?;
        studio.set_focus().map_err(|error| error.to_string())?;
        program.set_focusable(true).map_err(|error| {
            format!("Program konnte nicht für Fensterauswahl aktiviert werden: {error}")
        })?;
        program.set_skip_taskbar(false).map_err(|error| {
            let registration_error =
                format!("Program konnte nicht für Fensterauswahl registriert werden: {error}");
            match conceal_program_after_picker(&program, state.inner().surfaces.program) {
                Ok(()) => registration_error,
                Err(conceal_error) => format!("{registration_error}; {conceal_error}"),
            }
        })?;

        let stage_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match platform::stage_program_for_picker(
                state.inner().surfaces.program,
                state.inner().surfaces.studio,
            ) {
                Ok(()) => break,
                Err(_) if std::time::Instant::now() < stage_deadline => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    return Err(
                        match conceal_program_after_picker(&program, state.inner().surfaces.program)
                        {
                            Ok(()) => error,
                            Err(conceal_error) => format!("{error}; {conceal_error}"),
                        },
                    );
                }
            }
        }

        let generation = state
            .inner()
            .picker_stage_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        state
            .inner()
            .picker_stage_active
            .store(true, Ordering::Release);
        let generation_counter = state.inner().picker_stage_generation.clone();
        let active = state.inner().picker_stage_active.clone();
        let surfaces = state.inner().surfaces;
        let timer_app = app.clone();
        if let Err(error) = thread::Builder::new()
            .name("stream-picker-privacy".into())
            .spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(PICKER_SECONDS);
                let mut privacy_failure = None;
                while std::time::Instant::now() < deadline
                    && generation_counter.load(Ordering::Acquire) == generation
                {
                    if let Err(error) =
                        platform::assert_program_picker_covered(surfaces.program, surfaces.studio)
                    {
                        privacy_failure = Some(error);
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                if generation_counter.load(Ordering::Acquire) != generation {
                    return;
                }
                let restore_error = match timer_app.get_window("program") {
                    Some(program) => conceal_program_after_picker(&program, surfaces.program).err(),
                    None => platform::restore_program_offscreen(surfaces.program).err(),
                };
                active.store(false, Ordering::Release);
                let picker_error = match (privacy_failure, restore_error) {
                    (Some(privacy), Some(restore)) => Some(format!(
                        "{privacy}; Wiederherstellung fehlgeschlagen: {restore}"
                    )),
                    (Some(error), None) | (None, Some(error)) => Some(error),
                    (None, None) => None,
                };
                if let Some(error) = picker_error {
                    eprintln!("[hooviestar] sichere Fensterauswahl abgebrochen: {error}");
                    let _ = timer_app.emit("stream-picker-error", error);
                } else {
                    let _ = timer_app.emit("stream-picker-restored", ());
                }
            })
        {
            state
                .inner()
                .picker_stage_active
                .store(false, Ordering::Release);
            let spawn_error = format!(
                "Zeitgeber für sichere Fensterauswahl konnte nicht gestartet werden: {error}"
            );
            return Err(
                match conceal_program_after_picker(&program, state.inner().surfaces.program) {
                    Ok(()) => spawn_error,
                    Err(conceal_error) => format!("{spawn_error}; {conceal_error}"),
                },
            );
        }
        Ok(PICKER_SECONDS)
    }
}

fn activate_studio(app: &AppHandle) {
    if try_activate_studio(app) {
        return;
    }
    if STUDIO_ACTIVATION_RETRYING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    if let Err(error) = thread::Builder::new()
        .name("studio-activation".into())
        .spawn(move || {
            // Die Single-Instance-Meldung kann während des ersten Setup-Laufs
            // eintreffen. Begrenzt nach dem statisch konfigurierten Studio
            // suchen, ohne den Plugin-Callback oder UI-Thread zu blockieren.
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(50));
                if try_activate_studio(&app) {
                    STUDIO_ACTIVATION_RETRYING.store(false, Ordering::Release);
                    return;
                }
            }
            STUDIO_ACTIVATION_RETRYING.store(false, Ordering::Release);
            eprintln!("[hooviestar] second launch could not find Studio window");
        })
    {
        STUDIO_ACTIVATION_RETRYING.store(false, Ordering::Release);
        eprintln!("[hooviestar] failed to start Studio activation retry: {error}");
    }
}

fn try_activate_studio(app: &AppHandle) -> bool {
    let Some(studio) = app.get_webview_window("studio") else {
        return false;
    };
    if let Err(error) = studio.show() {
        eprintln!("[hooviestar] failed to show Studio after second launch: {error}");
    }
    if let Err(error) = studio.unminimize() {
        eprintln!("[hooviestar] failed to restore Studio after second launch: {error}");
    }
    if let Err(error) = studio.set_focus() {
        eprintln!("[hooviestar] failed to focus Studio after second launch: {error}");
    }
    true
}

#[cfg(target_os = "windows")]
fn show_startup_failure(message: &str) {
    use windows::{
        Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
        core::{PCWSTR, w},
    };
    let mut text: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(text.as_mut_ptr()),
            w!("Hooviestar konnte nicht gestartet werden"),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn show_startup_failure(_message: &str) {}

pub fn run() {
    platform::configure_graphics_backend();
    let resources = Arc::new(RuntimeResources::new());
    let setup_resources = resources.clone();
    let app = tauri::Builder::default()
        // Muss laut Plugin-Vertrag zuerst registriert werden. Eine zweite
        // Instanz darf keine weiteren Program-Fenster, Geräte oder Hotkeys
        // erzeugen; sie aktiviert nur das bestehende Studio.
        .plugin(tauri_plugin_single_instance::init(|app, _arguments, _cwd| {
            activate_studio(app);
        }))
        .plugin(
            tauri_plugin_window_state::Builder::default()
                // Auch zukünftige Webview-Fenster bleiben ausgeschlossen.
                // Sichtbarkeit und Fullscreen werden absichtlich nie restauriert.
                .with_filter(|label| label == "studio")
                .with_state_flags(
                    StateFlags::POSITION | StateFlags::SIZE | StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .manage(resources.clone())
        .manage(taskbar::TaskbarState::default())
        .manage(updater::UpdateState::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            dispatch,
            engine_status,
            enumerate_sources,
            select_portal_sources,
            canonicalize_file,
            set_preview_bounds,
            set_preview_overlay,
            set_preview_visible,
            prepare_stream_picker,
            updater::updater_status
        ])
        .setup(move |app| {
            let setup_result: Result<(), String> = (|| {
            let studio = app
                .get_webview_window("studio")
                .ok_or_else(|| "Studio window unavailable".to_string())?;
            // Die Program-Oberflaeche muss gemappt bleiben, damit Discord sie
            // als App-Fenster anbietet und kontinuierlich capturen kann. Die
            // Plattformintegration platziert sie vor dem ersten sichtbaren
            // Frame ausserhalb des physischen Desktops. Ein blosses
            // `visible(false)` wuerde das Fenster aus Discord entfernen.
            let output_visibility = platform::OutputVisibility::prepare()?;
            let program = WindowBuilder::new(app, "program")
                .title("Hooviestar – Program")
                .inner_size(1280.0, 720.0)
                .decorations(false)
                .focused(false)
                .focusable(false)
                .skip_taskbar(true)
                .visible(output_visibility.initially_visible())
                .build()
                .map_err(|error| error.to_string())?;
            output_visibility.show_program(&program)?;
            #[cfg(target_os = "windows")]
            {
                // Keep Tao's visibility state aligned with the raw Win32 mapping.
                // Without this, picker staging's set_focusable(true) hides the
                // still-captureable HWND because Tao believes it is invisible.
                program.show().map_err(|error| {
                    format!("Program-Fenster konnte nicht angezeigt werden: {error}")
                })?;
            }
            spawn_audio_watchdog()?;
            let (preview, surfaces) =
                NativePreview::create(&studio, &program, &output_visibility)?;
            let preview_handle = preview.native_handle();
            let engine = Arc::new(
                EngineHandle::start(surfaces, OutputConfig::default())
                    .map_err(|error| error.to_string())?,
            );
            #[cfg(target_os = "linux")]
            setup_resources.portal.set_link(engine.portal_link());
            let events = engine.take_events().map_err(|error| error.to_string())?;
            // Startzeit-Ereignisse stecken bereits im Kanal, bevor der
            // Forwarder-Thread und der Webview-Listener existieren; sie werden
            // gepuffert und von engine_status nachgeliefert.
            let initial_events = Mutex::new({
                let mut buffered = Vec::new();
                while let Ok(event) = events.try_recv() {
                    taskbar::record_engine_event(app.handle(), &event);
                    buffered.push(event);
                }
                buffered
            });
            app.manage(AppState {
                engine: engine.clone(),
                preview: preview_handle,
                surfaces,
                hotkey_mutations: Mutex::new(()),
                initial_hotkey_failures: Mutex::new(Vec::new()),
                initial_events,
                events_ready: AtomicBool::new(false),
                pending_events: Mutex::new(Vec::new()),
                #[cfg(target_os = "windows")]
                picker_stage_generation: Arc::new(AtomicU64::new(0)),
                #[cfg(target_os = "windows")]
                picker_stage_active: Arc::new(AtomicBool::new(false)),
            });
            register_initial_hotkeys(app.handle(), engine.clone());

            let handle = app.handle().clone();
            let stop_events = setup_resources.stop_events.clone();
            let event_thread = thread::Builder::new()
                .name("engine-events".into())
                .spawn(move || {
                    let mut printed_once = false;
                    while !stop_events.load(Ordering::Acquire) {
                        match events.recv_timeout(Duration::from_millis(100)) {
                            Ok(event) => {
                                taskbar::record_engine_event(&handle, &event);
                                let state = handle.state::<AppState>();
                                let mut pending = state
                                    .pending_events
                                    .lock()
                                    .expect("pending event mutex poisoned");
                                // Sperr-Reihenfolge gegen Stranding: Der Forwarder
                                // sperrt pending_events ZUERST und liest events_ready
                                // erst unter dieser Sperre neu. false: Der Push er-
                                // folgt unter derselben Sperre; engine_status sig-
                                // nalisiert Bereitschaft (Store) vor dem Entleeren
                                // und entleert unter eben dieser Sperre, sieht je-
                                // den zuvor gepushten Eintrag also sicher. true:
                                // Der Store liegt zurueck, das Ereignis umgeht den
                                // Puffer und geht direkt an den Emit-Pfad. Jedes
                                // Ereignis wird damit entweder entleert oder
                                // emittiert und kann nie im Puffer stranden.
                                if !state.events_ready.load(Ordering::Acquire) {
                                    let overflowed = pending.len() >= 512;
                                    if overflowed {
                                        pending.remove(0);
                                    }
                                    pending.push(event);
                                    drop(pending);
                                    if overflowed {
                                        // Nur beim Uebergang in den Ueberlauf loggen,
                                        // nicht Zeile fuer Zeile pro Verwurf.
                                        if !printed_once {
                                            eprintln!(
                                                "[hooviestar] pending event buffer full; dropped oldest"
                                            );
                                        }
                                        printed_once = true;
                                    } else {
                                        printed_once = false;
                                    }
                                } else {
                                    // Emit unter demselben Gate wie Replay:
                                    // Start-Ereignisse bleiben vor Live-Events.
                                    // Bei Fehler das aktuelle Event erhalten
                                    // und wieder in den Pufferbetrieb wechseln.
                                    if let Err(error) = handle.emit("engine-event", &event) {
                                        eprintln!("[hooviestar] failed to emit engine event: {error}");
                                        state.events_ready.store(false, Ordering::Release);
                                        if pending.len() >= 512 {
                                            pending.remove(0);
                                        }
                                        pending.push(event);
                                    }
                                }
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .map_err(|error| error.to_string())?;
            *setup_resources
                .engine
                .lock()
                .expect("engine mutex poisoned") = Some(engine);
            *setup_resources
                .preview
                .lock()
                .expect("preview mutex poisoned") = Some(preview);
            *setup_resources
                .output_visibility
                .lock()
                .expect("output visibility mutex poisoned") = Some(output_visibility);
            *setup_resources
                .event_thread
                .lock()
                .expect("event thread mutex poisoned") = Some(event_thread);
            updater::spawn(app.handle().clone());
                Ok(())
            })();
            if let Err(error) = &setup_result {
                let message = format!(
                    "Hooviestar konnte nicht gestartet werden.\n\n{error}\n\n\
                     Ohne einen initialisierten nativen Renderer wird kein gültiges \
                     Program-Fenster für die Freigabe angeboten."
                );
                show_startup_failure(&message);
            }
            setup_result.map_err(Into::into)
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| {
            let message = format!(
                "Hooviestar konnte nicht gestartet werden.\n\n{error}\n\n\
                 Ohne einen initialisierten nativen Renderer wird kein gültiges \
                 Program-Fenster für die Freigabe angeboten."
            );
            show_startup_failure(&message);
            panic!("Tauri build failed: {error}");
        });
    let app_handle = app.handle().clone();
    let run_resources = resources.clone();
    app.run(move |handle, event| match event {
        RunEvent::ExitRequested { .. } | RunEvent::Exit => run_resources.cleanup(handle),
        #[cfg(target_os = "linux")]
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::Resized(size),
            ..
        } if label == "program" || label == "preview" => {
            if let Some(state) = handle.try_state::<AppState>() {
                state
                    .inner()
                    .engine
                    .set_surface_size(&label, size.width, size.height);
            }
        }
        _ => {}
    });
    resources.cleanup(&app_handle);
}

#[cfg(target_os = "windows")]
fn spawn_audio_watchdog() -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let journal = hooviestar_engine::audio::journal::default_journal_path()
        .map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new(executable);
    command
        .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
        .arg("--audio-watchdog")
        .arg(std::process::id().to_string())
        .arg(journal)
        // Keep watchdog diagnostics on the parent's stderr without creating a
        // console window for this helper process.
        .stderr(std::process::Stdio::inherit());
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(not(target_os = "windows"))]
fn spawn_audio_watchdog() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn run_audio_watchdog(parent_process_id: u32, journal: &std::path::Path) {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };

    if let Ok(parent) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_process_id) } {
        unsafe { WaitForSingleObject(parent, INFINITE) };
        let _ = unsafe { CloseHandle(parent) };
    }
    let _ = hooviestar_engine::discovery::windows::repair_audio_journal(journal);
}

#[cfg(not(target_os = "windows"))]
pub fn run_audio_watchdog(_parent_process_id: u32, _journal: &std::path::Path) {}

fn register_initial_hotkeys(app: &AppHandle, engine: Arc<EngineHandle>) {
    let app_state = app.state::<AppState>();
    let initial_hotkey_failures = &app_state.inner().initial_hotkey_failures;
    let mut failures: Vec<String> = Vec::new();
    for scene in engine.snapshot().scenes {
        let Some(shortcut) = scene.hotkey else {
            continue;
        };
        if let Err(error) = register_hotkey(app, engine.clone(), scene.id, &shortcut) {
            failures.push(format!("scene {} [{shortcut}]: {error}", scene.id));
            initial_hotkey_failures
                .lock()
                .expect("hotkey mutex poisoned")
                .push((scene.id, format!("{shortcut}: {error}")));
        }
    }
    if !failures.is_empty() {
        eprintln!(
            "[hooviestar] failed to register {} initial scene hotkey(s): {}",
            failures.len(),
            failures.join("; ")
        );
    }
}

fn update_scene_hotkey(
    app: &AppHandle,
    engine: Arc<EngineHandle>,
    hotkey_mutations: &Mutex<()>,
    scene_id: Uuid,
    new_shortcut: Option<String>,
) -> Result<(), String> {
    let _hotkey_guard = hotkey_mutations.lock().expect("hotkey mutex poisoned");
    let old_shortcut = engine
        .snapshot()
        .scenes
        .into_iter()
        .find(|scene| scene.id == scene_id)
        .ok_or_else(|| "Szene nicht gefunden".to_string())?
        .hotkey;
    if old_shortcut == new_shortcut {
        // Identisches Neu-Speichern ist nur dann ein No-op, wenn die
        // Registrierung noch lebt; nach einem verlorenen Startkonflikt wird
        // sie hier erneuert, statt stumm Ok zu melden.
        if let Some(shortcut) = &new_shortcut
            && !app.global_shortcut().is_registered(shortcut.as_str())
            && let Err(error) = register_hotkey(app, engine.clone(), scene_id, shortcut)
        {
            let message = format!("{shortcut}: {error}");
            return Err(message);
        }
        return Ok(());
    }

    if let Some(shortcut) = &new_shortcut
        && let Err(error) = register_hotkey(app, engine.clone(), scene_id, shortcut)
    {
        let message = format!("{shortcut}: {error}");
        return Err(message);
    }
    let old_was_registered = old_shortcut
        .as_ref()
        .is_some_and(|shortcut| app.global_shortcut().is_registered(shortcut.as_str()));
    if old_was_registered && let Some(shortcut) = &old_shortcut {
        // Eine Alt-Registrierung, die beim Start einen Konflikt verlor, ist
        // nicht registriert und bleibt ein No-op. Ein echter Unregister-Fehler
        // bricht dagegen ab: sonst bliebe eine Ghost-Bindung aktiv.
        if let Err(error) = app.global_shortcut().unregister(shortcut.as_str()) {
            let rollback = new_shortcut.as_ref().and_then(|new_shortcut| {
                app.global_shortcut()
                    .unregister(new_shortcut.as_str())
                    .err()
                    .map(|rollback| {
                        format!(
                            "neuer Hotkey {new_shortcut} konnte nicht zurückgerollt werden: {rollback}"
                        )
                    })
            });
            let mut message =
                format!("Alter Hotkey {shortcut} konnte nicht entfernt werden: {error}");
            if let Some(rollback) = rollback {
                message.push_str(&format!("; {rollback}"));
            }
            return Err(message);
        }
    }

    let command = EngineCommand::SetSceneHotkey {
        scene_id,
        hotkey: new_shortcut.clone(),
    };
    if let Err(error) = engine.command(command) {
        let mut rollback_failures = Vec::new();
        if let Some(shortcut) = &new_shortcut
            && let Err(rollback) = app.global_shortcut().unregister(shortcut.as_str())
        {
            rollback_failures.push(format!(
                "neuer Hotkey {shortcut} konnte nicht entfernt werden: {rollback}"
            ));
        }
        if old_was_registered
            && let Some(shortcut) = &old_shortcut
            && let Err(rollback) = register_hotkey(app, engine, scene_id, shortcut)
        {
            rollback_failures.push(format!(
                "alter Hotkey {shortcut} konnte nicht wiederhergestellt werden: {rollback}"
            ));
        }
        let mut message = error.to_string();
        if !rollback_failures.is_empty() {
            message.push_str("; Hotkey-Rollback fehlgeschlagen: ");
            message.push_str(&rollback_failures.join("; "));
        }
        return Err(message);
    }
    Ok(())
}

fn register_hotkey(
    app: &AppHandle,
    engine: Arc<EngineHandle>,
    scene_id: Uuid,
    shortcut: &str,
) -> Result<(), String> {
    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            if event.state == ShortcutState::Pressed
                && let Err(error) = engine.command(EngineCommand::SetActiveScene { scene_id })
            {
                eprintln!("[hooviestar] hotkey scene switch failed for {scene_id}: {error}");
            }
        })
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::physical_preview_bounds;

    #[test]
    fn preview_bounds_scale_and_round_once() {
        assert_eq!(
            physical_preview_bounds(1.5, -10.4, 20.2, 100.0, 50.0),
            Ok((-16, 30, 150, 75))
        );
    }

    #[test]
    fn preview_bounds_reject_invalid_or_unrepresentable_geometry() {
        assert!(physical_preview_bounds(1.0, 0.0, 0.0, f64::NAN, 50.0).is_err());
        assert!(physical_preview_bounds(1.0, 0.0, 0.0, -1.0, 50.0).is_err());
        assert!(physical_preview_bounds(0.0, 0.0, 0.0, 100.0, 50.0).is_err());
        assert!(physical_preview_bounds(1.0, f64::MAX, 0.0, 100.0, 50.0).is_err());
        assert!(physical_preview_bounds(0.001, 0.0, 0.0, 1.0, 1.0).is_err());
    }
}
