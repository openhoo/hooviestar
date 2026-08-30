use std::{
    collections::HashMap,
    mem::{ManuallyDrop, size_of},
    path::{Path, PathBuf},
    slice,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
            AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
            ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
            IActivateAudioInterfaceCompletionHandler,
            IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
            IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, eConsole, eRender,
        },
        System::{
            Com::{
                BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
                StructuredStorage::{
                    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
                },
            },
            Threading::{CreateEventW, WaitForSingleObject},
            Variant::VT_BLOB,
        },
    },
    core::{HRESULT, IUnknown, Interface, Ref, implement},
};

use super::{GainRamp, LIMITER_CEILING, MediaAudioBus, PcmRing, SAMPLE_RATE, emit_availability};
use crate::{
    audio::journal::{RestoreJournal, SessionRestoreEntry, default_journal_path},
    discovery::windows::{repair_audio_journal, resolve_audio_process, restore_audio_session},
    engine::{AudioWarningKind, EngineEvent, LevelEntry},
    project::{AudioSessionBinding, ProjectV1, Source},
};
use uuid::Uuid;

/// Ticks (je 500 ms) mit aktivem Retry-Ausfall nach fehlgeschlagener Aktivierung.
const RETRY_TICKS_AFTER_FAILURES: u32 = 20;
/// Neustartversuche des Mischers bei unmittelbar wiederholten Fehlern.
const MIXER_MAX_CONSECUTIVE_RESTARTS: u32 = 5;
const MIXER_RESTART_BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const MIXER_RESTART_BACKOFF_MAX: Duration = Duration::from_secs(8);

/// Intervall des Verwaltungs-Threads, der Quellen synchronisiert.
const MANAGEMENT_SYNC_INTERVAL: Duration = Duration::from_millis(500);
/// Läuft ein Versuch länger als gesund, gilt die Fehlerkette als unterbrochen.
const MIXER_HEALTHY_RUNTIME: Duration = Duration::from_secs(30);

/// Ticks (je 500 ms) mit durchgehend leerem Ring, bevor ein gebundener,
/// lebender Capture einmalig Stagnation meldet.
const STALE_RING_TICKS: u32 = 30;

/// Begrenzt aufeinanderfolgende Warte-Timeouts des Render-Events (je 100 ms):
/// Rund 5 s ohne WASAPI-Ereignis bedeuten ein ungültig gewordenes Endgerät;
/// danach kehrt ein Fehler zum Supervisor-Neustart zurück statt stiller
/// Dauerschleife.
const WASAPI_MAX_WAIT_TIMEOUTS: u32 = 50;
/// Obergrenze gleichzeitig gemischter Eingaben (Parität zu Linux).
const MAX_MIX_INPUTS: usize = 16;
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    sender: StdMutex<Option<mpsc::Sender<Result<IAudioClient, String>>>>,
}

#[allow(non_snake_case)]
impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        operation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let result = (|| -> windows::core::Result<IAudioClient> {
            let operation = operation
                .as_ref()
                .ok_or_else(windows::core::Error::from_win32)?;
            let mut activation_result = HRESULT(0);
            let mut activated: Option<IUnknown> = None;
            unsafe { operation.GetActivateResult(&mut activation_result, &mut activated) }?;
            activation_result.ok()?;
            activated
                .ok_or_else(windows::core::Error::from_win32)?
                .cast::<IAudioClient>()
        })()
        .map_err(|error| error.to_string());
        if let Some(sender) = self
            .sender
            .lock()
            .expect("activation sender poisoned")
            .take()
        {
            let _ = sender.send(result);
        }
        Ok(())
    }
}

/// Räumt WASAPI-Client und Ereignishandle auf jedem Exit-Pfad auf.
struct StreamGuard {
    client: IAudioClient,
    event: HANDLE,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

pub struct ProcessAudioCapture {
    ring: Arc<Mutex<PcmRing>>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    failure: Arc<Mutex<Option<String>>>,
}

impl ProcessAudioCapture {
    pub fn start(process_id: u32) -> Result<Self, String> {
        if process_id == 0 {
            return Err("process id must be non-zero".into());
        }
        let ring = Arc::new(Mutex::new(PcmRing::new(SAMPLE_RATE as usize / 10)));
        let thread_ring = ring.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let failure = Arc::new(Mutex::new(None));
        let thread_failure = failure.clone();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(format!("process-loopback-{process_id}"))
            .spawn(move || {
                let result = capture_thread(process_id, thread_ring, thread_stop, ready_sender);
                if let Err(error) = result {
                    eprintln!("process loopback stopped: {error}");
                    *thread_failure.lock() = Some(error);
                }
            })
            .map_err(|error| error.to_string())?;
        match ready_receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Self {
                ring,
                stop,
                thread: Mutex::new(Some(thread)),
                failure,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(error) => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(error.to_string())
            }
        }
    }

    pub fn pop(&self) -> [f32; 2] {
        self.ring.lock().pop()
    }

    /// Grund des Fehlers, falls der Capture-Thread unerwartet geendet hat.
    pub fn failure_reason(&self) -> Option<String> {
        self.failure.lock().clone()
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

impl Drop for ProcessAudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.get_mut().take() {
            let _ = thread.join();
        }
    }
}

/// Übergabetresor für Quell-Handles zwischen Mischer-Inkarnationen: Ein
/// interner Neustart darf die Stummschaltung gebundener Sitzungen nicht
/// kurzzeitig aufheben (hörbares Leck während der Backoff-Zeit). Erst das
/// echte Herunterfahren (Supervisor-Stopp bzw. Runtime-Drop) stellt wieder her.
pub struct AudioRuntime {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    handle_vault: HandleVault,
}

impl AudioRuntime {
    pub fn start(
        project: Arc<parking_lot::RwLock<ProjectV1>>,
        events: mpsc::Sender<EngineEvent>,
        media_audio: MediaAudioBus,
    ) -> Result<Self, String> {
        let journal_path = default_journal_path().map_err(|error| error.to_string())?;
        // Journalproblem sichtbar machen: Unlesbares Journal (Quarantäne)
        // oder gescheiterte Ersatzschreibung — die Baselines sind verloren,
        // betroffene Sitzungen bleiben ggf. stumm. Der Pfad ist der
        // Quarantäne-Ort, das Originaljournal bzw. das nicht ersetzbare Journal.
        if let Some((error, quarantined)) = repair_audio_journal(&journal_path)? {
            let _ = events.send(EngineEvent::EngineError {
                message: format!(
                    "Problem im Audio-Wiederherstellungsjournal ({error}); \
                     betroffene Datei: {} – Sitzungen wurden nicht automatisch entstummt.",
                    quarantined.display()
                ),
            });
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let last_failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let thread_last_failure = last_failure.clone();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let handle_vault: HandleVault = Arc::new(Mutex::new(HashMap::new()));
        let thread_vault = handle_vault.clone();
        let thread = thread::Builder::new()
            .name("wasapi-mixer".into())
            .spawn(move || {
                let mut consecutive_failures: u32 = 0;
                let mut backoff = MIXER_RESTART_BACKOFF_INITIAL;
                // Meldung über ausgeschöpfte Neustarts nur einmal pro Fehlerkette.
                let mut exhausted_reported = false;
                while !thread_stop.load(Ordering::Acquire) {
                    let started = Instant::now();
                    let result = mixer_thread(
                        project.clone(),
                        events.clone(),
                        media_audio.clone(),
                        journal_path.clone(),
                        thread_stop.clone(),
                        ready_sender.clone(),
                        thread_vault.clone(),
                    );
                    match result {
                        Ok(()) => return, // Sauberer Stopp ueber das Stop-Flag.
                        Err(error) => {
                            *thread_last_failure.lock() = Some(error.clone());
                            eprintln!("WASAPI mixer stopped: {error}");
                            let _ = events.send(EngineEvent::AudioWarning {
                                kind: AudioWarningKind::DeviceInvalidated,
                                message: format!("Audio-Mischer wurde unerwartet beendet: {error}"),
                            });
                            if started.elapsed() >= MIXER_HEALTHY_RUNTIME {
                                consecutive_failures = 0;
                                backoff = MIXER_RESTART_BACKOFF_INITIAL;
                                exhausted_reported = false;
                            } else {
                                consecutive_failures += 1;
                            }
                            if consecutive_failures >= MIXER_MAX_CONSECUTIVE_RESTARTS
                                && !exhausted_reported
                            {
                                exhausted_reported = true;
                                let _ = events.send(EngineEvent::AudioWarning {
                                    kind: AudioWarningKind::DeviceInvalidated,
                                    message: "Audio-Mischer konnte nach wiederholten Fehlern \
                                        vorübergehend nicht neu gestartet werden; weitere \
                                        Versuche laufen im Hintergrund."
                                        .into(),
                                });
                            }
                            // Nie dauerhaft enden, solange nicht gestoppt
                            // wurde: Ein späteres Wiedereinstecken des
                            // Endgeräts belebt den Mischer erneut.
                            sleep_stoppable(&thread_stop, backoff);
                            backoff = (backoff * 2).min(MIXER_RESTART_BACKOFF_MAX);
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        match ready_receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Mutex::new(Some(thread)),
                handle_vault,
            }),
            Ok(Err(error)) => {
                // Der Thread wiederholt Fehler intern mit Backoff; hier
                // endgueltig anhalten, sonst blockiert das Join Minuten.
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(error)
            }
            Err(error) => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                // Echtgrund melden statt generischem Timeout, wenn der
                // Mischer vorher wiederholt scheiterte (z. B. fehlendes
                // Standardendgerät).
                let detail = last_failure.lock().take();
                Err(match detail {
                    Some(failure) => format!("audio mixer did not become ready: {failure}"),
                    None => error.to_string(),
                })
            }
        }
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
        // Tresor leeren: Ohne Nachfolge-Inkarnation stellen die Drops
        // der Handles die Sitzungen wieder her.
        self.handle_vault.lock().clear();
    }
}

impl Drop for AudioRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.get_mut().take() {
            let _ = thread.join();
        }
        self.handle_vault.lock().clear();
    }
}
/// Schläft die Dauer ab, bricht aber früh bei gesetztem Stopp-Flag aus.
fn sleep_stoppable(stop: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100).min(deadline - now));
    }
}

/// Wie `sleep_stoppable`, bricht aber bereits aus, sobald EINES der beiden
/// Flags gesetzt ist (Supervisor-Stopp oder Verwaltungs-Notstopp).
fn sleep_stoppable_either(first: &AtomicBool, second: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !first.load(Ordering::Acquire) && !second.load(Ordering::Acquire) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100).min(deadline - now));
    }
}

fn mixer_thread(
    project: Arc<parking_lot::RwLock<ProjectV1>>,
    events: mpsc::Sender<EngineEvent>,
    media_audio: MediaAudioBus,
    journal_path: PathBuf,
    stop: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<(), String>>,
    handle_vault: HandleVault,
) -> Result<(), String> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|error| error.to_string())?;
    let result = mixer_thread_initialized(
        project,
        events,
        media_audio,
        journal_path,
        stop,
        ready,
        handle_vault,
    );
    unsafe { CoUninitialize() };
    result
}

fn mixer_thread_initialized(
    project: Arc<parking_lot::RwLock<ProjectV1>>,
    events: mpsc::Sender<EngineEvent>,
    media_audio: MediaAudioBus,
    journal_path: PathBuf,
    stop: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<(), String>>,
    handle_vault: HandleVault,
) -> Result<(), String> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|error| error.to_string())?;
    let endpoint = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|error| error.to_string())?;
    let client: IAudioClient =
        unsafe { endpoint.Activate(CLSCTX_ALL, None) }.map_err(|error| error.to_string())?;
    let format = WAVEFORMATEX {
        wFormatTag: 3,
        nChannels: 2,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * 2 * 4,
        nBlockAlign: 8,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &format, None) }
        .map_err(|error| error.to_string())?;
    let event =
        unsafe { CreateEventW(None, false, false, None) }.map_err(|error| error.to_string())?;
    // Guard sofort nach CreateEventW: Auch ein fehlgeschlagenes
    // SetEventHandle darf das Handle nicht für die Prozesslaufzeit leaken.
    let stream = StreamGuard { client, event };
    unsafe { stream.client.SetEventHandle(stream.event) }.map_err(|error| error.to_string())?;
    let buffer_size =
        unsafe { stream.client.GetBufferSize() }.map_err(|error| error.to_string())?;
    let render: IAudioRenderClient =
        unsafe { stream.client.GetService() }.map_err(|error| error.to_string())?;
    unsafe { stream.client.Start() }.map_err(|error| error.to_string())?;

    // Die Quellenverwaltung läuft neben dem Render-Thread: Erzeugung,
    // Stummschaltung und Journal-I/O blockieren dort und dürfen den Mischer
    // nicht ausbremsen. Der Render-Thread sieht nur fertige Mischringe.
    let live: Arc<Mutex<HashMap<Uuid, Arc<LiveSource>>>> = Arc::new(Mutex::new(HashMap::new()));
    let management_events = events.clone();
    // Eigenes Notstopp-Flag für den Verwaltungs-Thread: Der Guard-Drop nach
    // einem Render-Fehler setzt NUR dieses Flag — fasst er den Stop-Arc des
    // Supervisors an, endet dessen Neustart-Schleife dauerhaft.
    let mgmt_stop = Arc::new(AtomicBool::new(false));
    let thread_mgmt_stop = mgmt_stop.clone();
    let management = ManagementGuard {
        mgmt_stop,
        handle: Some(
            thread::Builder::new()
                .name("audio-source-management".into())
                .spawn({
                    let live = live.clone();
                    let stop = stop.clone();
                    let vault = handle_vault;
                    move || {
                        management_loop(ManagementLoopContext {
                            project,
                            events: management_events,
                            media_audio,
                            journal_path,
                            live,
                            stop,
                            mgmt_stop: thread_mgmt_stop,
                            handle_vault: vault,
                        })
                    }
                })
                .map_err(|error| error.to_string())?,
        ),
    };
    let _ = ready.send(Ok(()));
    let _ = events.send(EngineEvent::AudioRecovered);

    let mut last_levels = Instant::now();
    // Aufeinanderfolgende Warte-Timeouts zählen: Ein gesundes Endgerät
    // meldet sich spätestens nach 100 ms.
    let mut wait_timeouts: u32 = 0;
    while !stop.load(Ordering::Acquire) {
        if unsafe { WaitForSingleObject(stream.event, 100) } != WAIT_OBJECT_0 {
            // Auch im Timeout die Gerätegesundheit prüfen: GetCurrentPadding
            // scheitert am invalidierten Gerät, `?` wandelt das in den
            // vorhandenen Supervisor-Neustart um.
            let _padding =
                unsafe { stream.client.GetCurrentPadding() }.map_err(|error| error.to_string())?;
            wait_timeouts += 1;
            if wait_timeouts >= WASAPI_MAX_WAIT_TIMEOUTS {
                return Err("WASAPI meldet keine Ereignisse mehr".into());
            }
            continue;
        }
        wait_timeouts = 0;
        let padding =
            unsafe { stream.client.GetCurrentPadding() }.map_err(|error| error.to_string())?;
        let frames = buffer_size.saturating_sub(padding);
        if frames == 0 {
            continue;
        }
        // Momentaufnahme der Mischseiten: kurze Sperre zum Klonen, danach
        // nur noch referenzgezählte Ringe anfassen.
        let snapshot: Vec<(Uuid, Arc<LiveSource>)> = live
            .lock()
            .iter()
            .map(|(source_id, source)| (*source_id, Arc::clone(source)))
            .collect();
        let data = unsafe { render.GetBuffer(frames) }.map_err(|error| error.to_string())?;
        let output = unsafe { slice::from_raw_parts_mut(data.cast::<f32>(), frames as usize * 2) };
        // Wie der Linux-Callback: Ring- und Rampen-Sperren einmal je Puffer
        // und Quelle nehmen, nie pro Frame. Pegel sammeln sich in
        // slot-indizierten Arrays statt einer je Frame gehashten Map.
        let mut rings: Vec<Option<parking_lot::MutexGuard<'_, PcmRing>>> =
            Vec::with_capacity(snapshot.len());
        let mut ramps: Vec<Option<parking_lot::MutexGuard<'_, GainRamp>>> =
            Vec::with_capacity(snapshot.len());
        for (_, shared) in &snapshot {
            rings.push(Some(shared.ring.lock()));
            ramps.push(Some(shared.gain.lock()));
        }
        let mut levels: Vec<(f32, f64, usize)> = vec![(0.0, 0.0, 0); snapshot.len()];
        for frame in output.as_chunks_mut::<2>().0 {
            let mut mixed = [0.0f32; 2];
            for (slot, (_, shared)) in snapshot.iter().enumerate() {
                let input = rings[slot].as_mut().map_or([0.0, 0.0], |ring| ring.pop());
                let [left, right] = accumulate_source(
                    &mut mixed,
                    input,
                    ramps[slot].as_deref_mut(),
                    shared.muted.load(Ordering::Relaxed),
                );
                let level = &mut levels[slot];
                level.0 = level.0.max(left.abs().max(right.abs()));
                level.1 += f64::from(left * left + right * right) * 0.5;
                level.2 += 1;
            }
            *frame = limit_stereo(mixed);
        }
        // Guards vor jeglichem blockierenden Aufruf ablegen.
        drop(rings);
        drop(ramps);
        unsafe { render.ReleaseBuffer(frames, 0) }.map_err(|error| error.to_string())?;
        if last_levels.elapsed() >= Duration::from_millis(100) {
            let entries = snapshot
                .iter()
                .zip(levels.iter())
                .map(|((source_id, _), (peak, squares, count))| LevelEntry {
                    source_id: *source_id,
                    peak: *peak,
                    rms: if *count == 0 {
                        0.0
                    } else {
                        (*squares / *count as f64).sqrt() as f32
                    },
                })
                .collect();
            let _ = events.send(EngineEvent::Levels { entries });
            last_levels = Instant::now();
        }
    }
    drop(management); // Verwaltungs-Thread stoppen und joinen, bevor COM endet.
    Ok(())
}

fn accumulate_source(
    mixed: &mut [f32; 2],
    input: [f32; 2],
    ramp: Option<&mut GainRamp>,
    muted: bool,
) -> [f32; 2] {
    // Preserve the runtime contract: volume ramps freeze while muted, then
    // resume click-free on unmute instead of progressing invisibly.
    let gain = if muted {
        0.0
    } else {
        ramp.map_or(0.0, |ramp| ramp.next_gain())
    };
    let contribution = [input[0] * gain, input[1] * gain];
    mixed[0] += contribution[0];
    mixed[1] += contribution[1];
    contribution
}

fn limit_stereo(mixed: [f32; 2]) -> [f32; 2] {
    let peak = mixed[0].abs().max(mixed[1].abs());
    let gain = if peak > LIMITER_CEILING {
        LIMITER_CEILING / peak
    } else {
        1.0
    };
    [mixed[0] * gain, mixed[1] * gain]
}

enum DesiredInput {
    Application(AudioSessionBinding),
    Media,
}

/// Verwaltungsseitiger Zustand einer Quelle: Besitzer des Capture-Threads
/// und der Wiederherstellungsdaten. Das Drop stoppt den Thread und hebt
/// die Stummschaltung der gebundenen Sitzung wieder auf.
struct SourceHandle {
    capture: Option<ProcessAudioCapture>,
    binding: Option<AudioSessionBinding>,
    /// Aufgelöste PID der Anwendungssitzung — Dedup-Schlüssel gegen doppelte Erfassung.
    pid: Option<u32>,
    target_volume: f32,
    restore: Option<SessionRestoreEntry>,
    journal_path: PathBuf,
}

impl SourceHandle {
    fn restore(&mut self) {
        let Some(entry) = self.restore.take() else {
            return;
        };
        if let Err(error) = restore_audio_session(&entry) {
            eprintln!(
                "Audio-Sitzung {} konnte nicht wiederhergestellt werden: {error}",
                entry.session_instance_id
            );
        } else if let Ok(mut journal) = RestoreJournal::load(&self.journal_path) {
            journal.remove(&entry.session_instance_id);
            let _ = journal.save_atomic(&self.journal_path);
        }
    }
}

impl Drop for SourceHandle {
    fn drop(&mut self) {
        // Feld leeren: Der Drop des Captures stoppt und joint den Thread.
        self.capture = None;
        self.restore();
    }
}

/// Tresor-Typ für die Übergabe lebender Quell-Handles an die nächste
/// Mischer-Inkarnation (siehe `AudioRuntime.handle_vault`).
type HandleVault = Arc<Mutex<HashMap<Uuid, SourceHandle>>>;

/// Mischseitige Sicht einer Quelle ohne blockierende Besitztümer.
struct LiveSource {
    ring: Arc<Mutex<PcmRing>>,
    gain: Mutex<GainRamp>,
    muted: AtomicBool,
}

/// Stoppt und joint den Verwaltungs-Thread auf jedem Exit-Pfad des
/// Render-Threads — auch bei Fehlerausgängen über `?`. Das Flag gehört
/// ausschließlich dem Verwaltungs-Thread (`mgmt_stop`): Würde der Drop den
/// Supervisor-Stop anfassen, beendete jeder Render-Fehler die Neustart-
/// Schleife des Supervisors dauerhaft.
struct ManagementGuard {
    mgmt_stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for ManagementGuard {
    fn drop(&mut self) {
        self.mgmt_stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Retry-Sperre gilt nur für die Bindung, deren Aktivierung scheiterte;
/// nach einem Retarget auf eine andere Bindung verliert sie ihre Wirkung.
struct CooldownEntry {
    remaining: u32,
    binding: Option<AudioSessionBinding>,
}

struct SourceManagementContext<'a> {
    events: &'a mpsc::Sender<EngineEvent>,
    media_audio: &'a MediaAudioBus,
    journal_path: &'a Path,
    live: &'a Mutex<HashMap<Uuid, Arc<LiveSource>>>,
}

#[derive(Default)]
struct SourceManagementState {
    handles: HashMap<Uuid, SourceHandle>,
    availability: HashMap<Uuid, bool>,
    retry_cooldown: HashMap<Uuid, CooldownEntry>,
    last_overruns: HashMap<Uuid, u64>,
    stale_rings: HashMap<Uuid, (u32, bool)>,
}

fn synchronize_sources(
    desired: &HashMap<Uuid, (DesiredInput, f32, bool)>,
    context: &SourceManagementContext<'_>,
    state: &mut SourceManagementState,
) {
    let SourceManagementState {
        handles,
        availability,
        retry_cooldown,
        last_overruns,
        stale_rings,
    } = state;
    let SourceManagementContext {
        events,
        media_audio,
        journal_path,
        live,
    } = context;
    handles.retain(|source_id, _| desired.contains_key(source_id));
    availability.retain(|source_id, _| desired.contains_key(source_id));
    retry_cooldown.retain(|source_id, _| desired.contains_key(source_id));
    live.lock()
        .retain(|source_id, _| desired.contains_key(source_id));
    // Gestorbene Capture-Threads räumen Verwaltungs- UND Mischseite auf.
    handles.retain(|source_id, handle| {
        let Some(capture) = &handle.capture else {
            return true;
        };
        let Some(reason) = capture.failure_reason() else {
            return true;
        };
        emit_availability(
            events,
            availability,
            *source_id,
            false,
            &format!("Prozess-Loopback wurde beendet: {reason}"),
        );
        retry_cooldown.insert(
            *source_id,
            CooldownEntry {
                remaining: RETRY_TICKS_AFTER_FAILURES,
                binding: handle.binding.clone(),
            },
        );
        live.lock().remove(source_id);
        false
    });
    for (source_id, (desired_input, volume, muted)) in desired {
        let desired_binding = match desired_input {
            DesiredInput::Application(binding) => Some(binding),
            DesiredInput::Media => None,
        };
        if let Some(handle) = handles.get_mut(source_id)
            && handle.binding.as_ref() == desired_binding
        {
            if let Some(shared) = live.lock().get(source_id) {
                if (handle.target_volume - *volume).abs() > f32::EPSILON {
                    shared.gain.lock().set(*volume, 480);
                    handle.target_volume = *volume;
                }
                shared.muted.store(*muted, Ordering::Relaxed);
            }
            continue;
        }
        // Neue Quelle oder geänderte Bindung: Der alte Capture bleibt bis
        // zur Fertigstellung des neuen bestehen — kein selbst verursachter
        // Ausfall, wenn die Aktivierung scheitert oder eine Sperre läuft.
        if matches!(desired_input, DesiredInput::Application(_)) {
            let blocking = match retry_cooldown.get(source_id) {
                None => false,
                Some(entry) => entry.binding.as_ref() == desired_binding && entry.remaining > 0,
            };
            if blocking {
                if let Some(entry) = retry_cooldown.get_mut(source_id) {
                    entry.remaining -= 1;
                }
                // Auch während der Sperre folgt der weitermischende
                // Fallback Lautstärke und Stummschaltung (Parität
                // zum Bindungstreffer oben).
                if let Some(handle) = handles.get_mut(source_id)
                    && let Some(shared) = live.lock().get(source_id)
                {
                    if (handle.target_volume - *volume).abs() > f32::EPSILON {
                        shared.gain.lock().set(*volume, 480);
                        handle.target_volume = *volume;
                    }
                    shared.muted.store(*muted, Ordering::Relaxed);
                }
                continue;
            }
            // Abgelaufene oder nicht mehr zutreffende Sperre entfernen.
            retry_cooldown.remove(source_id);
        }
        // Kapazitätsgrenze für neue Quellen (Parität zu Linux); bestehende
        // Quellen erhalten weiter Lautstärke-/Stummschaltungs-Updates.
        if !handles.contains_key(source_id) && handles.len() >= MAX_MIX_INPUTS {
            emit_availability(
                events,
                availability,
                *source_id,
                false,
                "Mixer-Kapazität erreicht",
            );
            continue;
        }
        let prepared = match desired_input {
            DesiredInput::Application(binding) => (|| {
                let pid = resolve_audio_process(binding)?;
                // Dedup: Dieselbe Sitzung nicht doppelt erfassen — der
                // Mischer würde beide Ringe summieren (+6 dB Echo).
                if handles
                    .iter()
                    .any(|(other_id, handle)| *other_id != *source_id && handle.pid == Some(pid))
                {
                    return Err(format!(
                        "Audio-Sitzung (PID {pid}) wird bereits von einer anderen Quelle erfasst"
                    ));
                }
                let capture = ProcessAudioCapture::start(pid)?;
                let ring = capture.ring.clone();
                // Windows process-loopback samples are post-session-mute on
                // current Windows 11. Muting the source session here makes
                // this ring silent and defeats the mixer. Leave the original
                // session audible; Discord application sharing captures this
                // process's separately rendered mixed session.
                Ok((Some(capture), ring, None, Some(pid)))
            })(),
            DesiredInput::Media => media_audio
                .lock()
                .get(source_id)
                .cloned()
                .map(|ring| (None, ring, None, None))
                .ok_or_else(|| "media audio stream is not ready".to_string()),
        };
        match prepared {
            Ok((capture, ring, restore, pid)) => {
                retry_cooldown.remove(source_id);
                let shared = Arc::new(LiveSource {
                    ring,
                    gain: Mutex::new(GainRamp::new(*volume)),
                    muted: AtomicBool::new(*muted),
                });
                let replaced = handles.insert(
                    *source_id,
                    SourceHandle {
                        capture,
                        binding: desired_binding.cloned(),
                        pid,
                        target_volume: *volume,
                        restore,
                        journal_path: journal_path.to_path_buf(),
                    },
                );
                // Bind-Rückstand verwerfen, bevor die Quelle gemischt wird:
                // Alter Backlog würde sonst als A-V-Versatz abgespielt.
                shared.ring.lock().clear();
                live.lock().insert(*source_id, shared);
                // Alter Capture erst jetzt verwerfen: Sein Drop stoppt
                // seinen Thread und stellt die alte Sitzung wieder her.
                drop(replaced);
                // Frische Bindung: Telemetrie-Baselines zurücksetzen,
                // damit Stagnation nicht vom Vorgänger-Capture geerbt wird.
                last_overruns.remove(source_id);
                stale_rings.remove(source_id);
                emit_availability(events, availability, *source_id, true, "");
            }
            Err(reason) => {
                if matches!(desired_input, DesiredInput::Application(_)) {
                    retry_cooldown.insert(
                        *source_id,
                        CooldownEntry {
                            remaining: RETRY_TICKS_AFTER_FAILURES,
                            binding: desired_binding.cloned(),
                        },
                    );
                }
                // Fehlgeschlagene Bindungsänderung: Der vorherige Capture
                // bleibt in handles/live bestehen und mischt unverändert
                // weiter — gestorbene Captures wurden oben bereits
                // ausgetragen, ein verbleibender Eintrag ist also live.
                // Die Quelle bleibt hörbar und wird zu Recht als
                // verfügbar geführt; nur ohne jeden Ersatz-Capture gilt
                // sie tatsächlich als ausgefallen.
                if handles.contains_key(source_id) {
                    eprintln!(
                        "Audio-Bindung konnte nicht gewechselt werden ({reason}); \
                         vorherige Quelle läuft weiter"
                    );
                } else {
                    emit_availability(events, availability, *source_id, false, &reason);
                }
            }
        }
    }
}

/// Telemetrie-Parität zu Linux: Überläufe werden sofort gemeldet — auch für
/// Medienquellen ohne eigenen Capture, deren Bus-Ring unabhängig vom Mix
/// weiterdekodiert; beim ersten Sichtbarwerden zählt nur der Zuwachs ab dem
/// aktuellen Ringstand, nicht die Vorgeschichte. Leere Ringe gelten bei
/// Prozess-Loopback als normal (jede Periode wird gepoppt) und erzeugen
/// keine Unterlauf-Warnung; ein gebundener, lebender Capture mit dauerhaft
/// leerem Ring meldet dagegen einmalig Stagnation, bis er wieder Frames
/// liefert. Für Medienquellen existiert kein Producer-Liveness-Signal,
/// daher entfallen Stagnation und Unterlauf dort bewusst.
fn report_ring_health(
    events: &mpsc::Sender<EngineEvent>,
    handles: &HashMap<Uuid, SourceHandle>,
    live: &Mutex<HashMap<Uuid, Arc<LiveSource>>>,
    last_overruns: &mut HashMap<Uuid, u64>,
    stale: &mut HashMap<Uuid, (u32, bool)>,
) {
    for (source_id, handle) in handles {
        // Medienquellen ohne eigenen Capture besitzen keinen Producer-
        // Liveness-Indikator; für sie wird ausschließlich der Überlauf-
        // Delta aus dem Bus-Ring gemeldet.
        let (overruns, filled_frames) = match &handle.capture {
            Some(capture) => {
                let ring = capture.ring.lock();
                (ring.overruns(), ring.filled_frames())
            }
            None => {
                let Some(source) = live.lock().get(source_id).cloned() else {
                    continue;
                };
                let ring = source.ring.lock();
                (ring.overruns(), 0)
            }
        };
        // Baseline auf den Ist-Stand setzen: Frische Captures starten bei
        // null; Medienringe dekodieren dagegen auch unvermischt weiter,
        // ihr Vorschub vor der Mischung ist keine neue Warnung.
        let last = last_overruns.entry(*source_id).or_insert(overruns);
        let new_overruns = overruns.saturating_sub(*last);
        *last = overruns;
        if new_overruns > 0 {
            let quelle = match &handle.capture {
                Some(_) => match &handle.binding {
                    Some(binding) => {
                        format!("Prozess-Loopback-Quelle „{}“", binding.process_path)
                    }
                    None => "Prozess-Loopback-Quelle".to_string(),
                },
                None => "Medienquelle".to_string(),
            };
            let _ = events.send(EngineEvent::AudioWarning {
                kind: AudioWarningKind::Overrun,
                message: format!(
                    "Tonpuffer-Überlauf für {quelle} ({new_overruns} Frames verworfen)"
                ),
            });
        }
        let Some(capture) = &handle.capture else {
            continue;
        };
        if capture.failure_reason().is_some() || filled_frames > 0 {
            stale.remove(source_id);
            continue;
        }
        let state = stale.entry(*source_id).or_insert((0, false));
        state.0 = state.0.saturating_add(1);
        if state.0 > STALE_RING_TICKS && !state.1 {
            state.1 = true;
            let _ = events.send(EngineEvent::AudioWarning {
                kind: AudioWarningKind::Underrun,
                message: match &handle.binding {
                    Some(binding) => format!(
                        "Quelle „{}“ liefert seit längerer Zeit keine Frames; Capture läuft weiter",
                        binding.process_path
                    ),
                    None => "Quelle liefert seit längerer Zeit keine Frames; Capture läuft weiter"
                        .into(),
                },
            });
        }
    }
    last_overruns.retain(|source_id, _| handles.contains_key(source_id));
    stale.retain(|source_id, _| handles.contains_key(source_id));
}

/// Eigenständiger Verwaltungs-Thread: spiegelt den Projekt-Wunschzustand in
/// fertige Mischringe, ohne den Render-Thread zu blockieren.
struct ManagementLoopContext {
    project: Arc<parking_lot::RwLock<ProjectV1>>,
    events: mpsc::Sender<EngineEvent>,
    media_audio: MediaAudioBus,
    journal_path: PathBuf,
    live: Arc<Mutex<HashMap<Uuid, Arc<LiveSource>>>>,
    stop: Arc<AtomicBool>,
    mgmt_stop: Arc<AtomicBool>,
    handle_vault: HandleVault,
}

fn management_loop(context: ManagementLoopContext) {
    let ManagementLoopContext {
        project,
        events,
        media_audio,
        journal_path,
        live,
        stop,
        mgmt_stop,
        handle_vault,
    } = context;
    // Noch lebende Handles der Vorgänger-Inkarnation übernehmen: Die
    // Capture-Threads laufen weiter, die Stummschaltung bleibt bestehen.
    let mut source_state = SourceManagementState {
        handles: std::mem::take(&mut *handle_vault.lock()),
        ..SourceManagementState::default()
    };
    let source_context = SourceManagementContext {
        events: &events,
        media_audio: &media_audio,
        journal_path: &journal_path,
        live: &live,
    };
    // Endet beim Supervisor-Stopp ODER beim eigenen Notstopp des Guards
    // (nach Render-Fehler); nur ersterer bedeutet echtes Herunterfahren.
    while !stop.load(Ordering::Acquire) && !mgmt_stop.load(Ordering::Acquire) {
        // Wunschzustand kurz unter dem Read-Lock kopieren; das eigentliche
        // Synchronisieren blockiert und darf den Lock nicht halten.
        let desired: HashMap<Uuid, (DesiredInput, f32, bool)> = {
            let project = project.read();
            project
                .sources
                .iter()
                .filter_map(|source| match source {
                    Source::ApplicationAudio {
                        id,
                        binding,
                        volume,
                        muted,
                        ..
                    } => Some((
                        *id,
                        (DesiredInput::Application(binding.clone()), *volume, *muted),
                    )),
                    Source::Media {
                        id, volume, muted, ..
                    } => Some((*id, (DesiredInput::Media, *volume, *muted))),
                    _ => None,
                })
                .collect()
        };
        synchronize_sources(&desired, &source_context, &mut source_state);
        report_ring_health(
            &events,
            &source_state.handles,
            &live,
            &mut source_state.last_overruns,
            &mut source_state.stale_rings,
        );
        sleep_stoppable_either(&stop, &mgmt_stop, MANAGEMENT_SYNC_INTERVAL);
    }
    // Echtes Herunterfahren (Supervisor-Stopp): Drops stoppen die Capture-
    // Threads und stellen die Sitzungen wieder her. Notstopp nach internem
    // Render-Fehler dagegen: Handles in den Tresor legen, damit die nächste
    // Inkarnation sie übernimmt, ohne die Stummschaltung kurzzeitig
    // aufzuheben. Übernimmt keine Inkarnation mehr (ausgeschöpfte Neustarts),
    // räumt AudioRuntime beim Stopp/Drop den Tresor aus.
    if stop.load(Ordering::Acquire) {
        source_state.handles.clear();
    } else {
        *handle_vault.lock() = source_state.handles;
    }
}

fn capture_thread(
    process_id: u32,
    ring: Arc<Mutex<PcmRing>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let co_init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|error| error.to_string());
    // Auch das Scheitern von CoInitializeEx muss den wartenden Aufrufer
    // erreichen, sonst blockt ProcessAudioCapture::start bis zum vollen
    // Ready-Timeout.
    if let Err(error) = co_init {
        let _ = ready.send(Err(error.clone()));
        return Err(error);
    }
    let result = capture_thread_initialized(process_id, ring, stop, ready);
    unsafe { CoUninitialize() };
    result
}

fn capture_thread_initialized(
    process_id: u32,
    ring: Arc<Mutex<PcmRing>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let client = match activate_process_loopback(process_id) {
        Ok(client) => client,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    // Der gesamte Setup nach der Aktivierung meldet Bereitschaft: Fehler
    // hier erreichten den Aufrufer zuvor nie und ließen
    // ProcessAudioCapture::start bis zum vollen Ready-Timeout (15 s) warten.
    let setup = (|| -> Result<(StreamGuard, IAudioCaptureClient), String> {
        let format = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 2,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 2 * 2,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &format, None) }
            .map_err(|error| error.to_string())?;
        let event =
            unsafe { CreateEventW(None, false, false, None) }.map_err(|error| error.to_string())?;
        // Guard sofort nach CreateEventW: Auch ein fehlgeschlagenes
        // SetEventHandle darf das Handle nicht für die Prozesslaufzeit leaken.
        let stream = StreamGuard { client, event };
        unsafe { stream.client.SetEventHandle(stream.event) }.map_err(|error| error.to_string())?;
        let capture: IAudioCaptureClient =
            unsafe { stream.client.GetService() }.map_err(|error| error.to_string())?;
        unsafe { stream.client.Start() }.map_err(|error| error.to_string())?;
        Ok((stream, capture))
    })();
    let (stream, capture) = match setup {
        Ok(parts) => parts,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    let _ = ready.send(Ok(()));
    // Aufeinanderfolgende Warte-Timeouts zählen: Ein gesunder Prozess-
    // Loopback meldet sich spätestens nach 100 ms.
    let mut wait_timeouts: u32 = 0;
    while !stop.load(Ordering::Acquire) {
        if unsafe { WaitForSingleObject(stream.event, 100) } != WAIT_OBJECT_0 {
            wait_timeouts += 1;
            if wait_timeouts >= WASAPI_MAX_WAIT_TIMEOUTS {
                return Err("Prozess-Loopback meldet keine Ereignisse mehr".into());
            }
            continue;
        }
        wait_timeouts = 0;
        loop {
            let frames =
                unsafe { capture.GetNextPacketSize() }.map_err(|error| error.to_string())?;
            if frames == 0 {
                break;
            }
            let mut data = std::ptr::null_mut();
            let mut frame_count = 0;
            let mut buffer_flags = 0;
            unsafe {
                capture.GetBuffer(&mut data, &mut frame_count, &mut buffer_flags, None, None)
            }
            .map_err(|error| error.to_string())?;
            if buffer_flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                let mut output = ring.lock();
                for _ in 0..frame_count {
                    output.push([0.0, 0.0]);
                }
            } else {
                let samples =
                    unsafe { slice::from_raw_parts(data.cast::<i16>(), frame_count as usize * 2) };
                let mut output = ring.lock();
                for sample in samples.as_chunks::<2>().0 {
                    output.push([
                        f32::from(sample[0]) / 32_768.0,
                        f32::from(sample[1]) / 32_768.0,
                    ]);
                }
            }
            unsafe { capture.ReleaseBuffer(frame_count) }.map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn activate_process_loopback(process_id: u32) -> Result<IAudioClient, String> {
    let mut activation = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: process_id,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let property = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&mut activation as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
                    },
                },
            }),
        },
    });
    let (sender, receiver) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        sender: StdMutex::new(Some(sender)),
    }
    .into();
    let _operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*property),
            &handler,
        )
    }
    .map_err(|error| error.to_string())?;
    receiver
        .recv_timeout(Duration::from_secs(10))
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::{LIMITER_CEILING, accumulate_source, limit_stereo};

    #[test]
    fn mixer_sums_independent_stereo_sources_with_gain() {
        let mut mixed = [0.0, 0.0];
        let mut first = super::GainRamp::new(0.5);
        assert_eq!(
            accumulate_source(&mut mixed, [0.8, -0.4], Some(&mut first), false),
            [0.4, -0.2]
        );
        let mut second = super::GainRamp::new(0.25);
        assert_eq!(
            accumulate_source(&mut mixed, [0.2, 0.6], Some(&mut second), false),
            [0.05, 0.15]
        );
        assert!((mixed[0] - 0.45).abs() < 1.0e-6);
        assert!((mixed[1] + 0.05).abs() < 1.0e-6);
    }

    #[test]
    fn muted_source_contributes_neither_audio_nor_meter_level() {
        let mut mixed = [0.25, -0.25];
        let mut ramp = super::GainRamp::new(0.0);
        ramp.set(0.75, 10);
        let contribution = accumulate_source(&mut mixed, [1.0, 1.0], Some(&mut ramp), true);
        assert_eq!(contribution, [0.0, 0.0]);
        assert_eq!(mixed, [0.25, -0.25]);
        let resumed = accumulate_source(&mut mixed, [1.0, 1.0], Some(&mut ramp), false);
        assert!((resumed[0] - 0.075).abs() < 1.0e-6);
        assert!((resumed[1] - 0.075).abs() < 1.0e-6);
    }

    #[test]
    fn limiter_preserves_stereo_ratio_and_caps_peak() {
        let limited = limit_stereo([2.0, -1.0]);
        assert!((limited[0] - LIMITER_CEILING).abs() < 1.0e-6);
        assert!((limited[1] + LIMITER_CEILING * 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn limiter_leaves_safe_mix_bit_exact() {
        assert_eq!(limit_stereo([0.25, -0.5]), [0.25, -0.5]);
    }
}
