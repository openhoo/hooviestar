use std::{
    collections::HashMap,
    mem::{ManuallyDrop, size_of},
    path::PathBuf,
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
        Foundation::{CloseHandle, WAIT_OBJECT_0},
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

use super::{GainRamp, LIMITER_CEILING, MediaAudioBus, PcmRing, SAMPLE_RATE};
use crate::{
    audio::journal::{RestoreJournal, SessionRestoreEntry, default_journal_path},
    discovery::windows::{
        mute_audio_session, repair_audio_journal, resolve_audio_process, restore_audio_session,
    },
    engine::{EngineEvent, LevelEntry},
    project::{ProjectV1, Source},
};
use uuid::Uuid;

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

pub struct ProcessAudioCapture {
    ring: Arc<Mutex<PcmRing>>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl ProcessAudioCapture {
    pub fn start(process_id: u32) -> Result<Self, String> {
        if process_id == 0 {
            return Err("process id must be non-zero".into());
        }
        let ring = Arc::new(Mutex::new(PcmRing::new(SAMPLE_RATE as usize * 2)));
        let thread_ring = ring.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(format!("process-loopback-{process_id}"))
            .spawn(move || {
                let result = capture_thread(process_id, thread_ring, thread_stop, ready_sender);
                if let Err(error) = result {
                    eprintln!("process loopback stopped: {error}");
                }
            })
            .map_err(|error| error.to_string())?;
        match ready_receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Self {
                ring,
                stop,
                thread: Mutex::new(Some(thread)),
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

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.lock().take() {
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

enum AudioInput {
    Process(ProcessAudioCapture),
    Media(Arc<Mutex<PcmRing>>),
}

impl AudioInput {
    fn pop(&self) -> [f32; 2] {
        match self {
            Self::Process(capture) => capture.pop(),
            Self::Media(ring) => ring.lock().pop(),
        }
    }

    fn shutdown(&self) {
        if let Self::Process(capture) = self {
            capture.shutdown();
        }
    }
}

struct SourceCapture {
    input: AudioInput,
    gain: GainRamp,
    target_volume: f32,
    muted: bool,
    restore: Option<SessionRestoreEntry>,
    journal_path: PathBuf,
}

impl SourceCapture {
    fn restore(&mut self) {
        let Some(entry) = self.restore.take() else {
            return;
        };
        if restore_audio_session(&entry).is_ok()
            && let Ok(mut journal) = RestoreJournal::load(&self.journal_path)
        {
            journal.remove(&entry.session_instance_id);
            let _ = journal.save_atomic(&self.journal_path);
        }
    }
}

impl Drop for SourceCapture {
    fn drop(&mut self) {
        self.input.shutdown();
        self.restore();
    }
}

pub struct AudioRuntime {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl AudioRuntime {
    pub fn start(
        project: Arc<parking_lot::RwLock<ProjectV1>>,
        events: mpsc::Sender<EngineEvent>,
        media_audio: MediaAudioBus,
    ) -> Result<Self, String> {
        let journal_path = default_journal_path().map_err(|error| error.to_string())?;
        repair_audio_journal(&journal_path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("wasapi-mixer".into())
            .spawn(move || {
                let result = mixer_thread(
                    project,
                    events,
                    media_audio,
                    journal_path,
                    thread_stop,
                    ready_sender,
                );
                if let Err(error) = result {
                    eprintln!("WASAPI mixer stopped: {error}");
                }
            })
            .map_err(|error| error.to_string())?;
        match ready_receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Mutex::new(Some(thread)),
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

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.lock().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AudioRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.get_mut().take() {
            let _ = thread.join();
        }
    }
}

fn mixer_thread(
    project: Arc<parking_lot::RwLock<ProjectV1>>,
    events: mpsc::Sender<EngineEvent>,
    media_audio: MediaAudioBus,
    journal_path: PathBuf,
    stop: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|error| error.to_string())?;
    let result = mixer_thread_initialized(project, events, media_audio, journal_path, stop, ready);
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
    unsafe { client.SetEventHandle(event) }.map_err(|error| error.to_string())?;
    let buffer_size = unsafe { client.GetBufferSize() }.map_err(|error| error.to_string())?;
    let render: IAudioRenderClient =
        unsafe { client.GetService() }.map_err(|error| error.to_string())?;
    unsafe { client.Start() }.map_err(|error| error.to_string())?;
    let _ = ready.send(Ok(()));

    let mut sources: HashMap<Uuid, SourceCapture> = HashMap::new();
    let mut last_sync = Instant::now() - Duration::from_secs(2);
    let mut last_levels = Instant::now();
    while !stop.load(Ordering::Acquire) {
        if last_sync.elapsed() >= Duration::from_secs(1) {
            synchronize_sources(
                &project.read(),
                &events,
                &media_audio,
                &journal_path,
                &mut sources,
            );
            last_sync = Instant::now();
        }
        if unsafe { WaitForSingleObject(event, 100) } != WAIT_OBJECT_0 {
            continue;
        }
        let padding = unsafe { client.GetCurrentPadding() }.map_err(|error| error.to_string())?;
        let frames = buffer_size.saturating_sub(padding);
        if frames == 0 {
            continue;
        }
        let data = unsafe { render.GetBuffer(frames) }.map_err(|error| error.to_string())?;
        let output = unsafe { slice::from_raw_parts_mut(data.cast::<f32>(), frames as usize * 2) };
        let mut peaks: HashMap<Uuid, (f32, f64, usize)> = HashMap::new();
        for frame in output.chunks_exact_mut(2) {
            let mut mixed = [0.0f32; 2];
            for (source_id, source) in &mut sources {
                let input = source.input.pop();
                let gain = if source.muted {
                    0.0
                } else {
                    source.gain.next_gain()
                };
                let left = input[0] * gain;
                let right = input[1] * gain;
                mixed[0] += left;
                mixed[1] += right;
                let level = peaks.entry(*source_id).or_default();
                level.0 = level.0.max(left.abs().max(right.abs()));
                level.1 += f64::from(left * left + right * right) * 0.5;
                level.2 += 1;
            }
            let peak = mixed[0].abs().max(mixed[1].abs());
            let limiter = if peak > LIMITER_CEILING {
                LIMITER_CEILING / peak
            } else {
                1.0
            };
            frame[0] = mixed[0] * limiter;
            frame[1] = mixed[1] * limiter;
        }
        unsafe { render.ReleaseBuffer(frames, 0) }.map_err(|error| error.to_string())?;
        if last_levels.elapsed() >= Duration::from_millis(100) {
            let entries = peaks
                .into_iter()
                .map(|(source_id, (peak, squares, count))| LevelEntry {
                    source_id,
                    peak,
                    rms: if count == 0 {
                        0.0
                    } else {
                        (squares / count as f64).sqrt() as f32
                    },
                })
                .collect();
            let _ = events.send(EngineEvent::Levels { entries });
            last_levels = Instant::now();
        }
    }
    for source in sources.values() {
        source.input.shutdown();
    }
    let _ = unsafe { client.Stop() };
    let _ = unsafe { CloseHandle(event) };
    Ok(())
}

enum DesiredInput<'a> {
    Application(&'a crate::project::AudioSessionBinding),
    Media,
}

fn synchronize_sources(
    project: &ProjectV1,
    events: &mpsc::Sender<EngineEvent>,
    media_audio: &MediaAudioBus,
    journal_path: &PathBuf,
    sources: &mut HashMap<Uuid, SourceCapture>,
) {
    let desired: HashMap<Uuid, (DesiredInput<'_>, f32, bool)> = project
        .sources
        .iter()
        .filter_map(|source| match source {
            Source::ApplicationAudio {
                id,
                binding,
                volume,
                muted,
                ..
            } => Some((*id, (DesiredInput::Application(binding), *volume, *muted))),
            Source::Media {
                id, volume, muted, ..
            } => Some((*id, (DesiredInput::Media, *volume, *muted))),
            _ => None,
        })
        .collect();
    sources.retain(|source_id, _| desired.contains_key(source_id));
    for (source_id, (desired_input, volume, muted)) in desired {
        if let Some(source) = sources.get_mut(&source_id) {
            if (source.target_volume - volume).abs() > f32::EPSILON {
                source.gain.set(volume, 480);
                source.target_volume = volume;
            }
            source.muted = muted;
            continue;
        }
        let prepared = match desired_input {
            DesiredInput::Application(binding) => (|| {
                let capture = ProcessAudioCapture::start(resolve_audio_process(binding)?)?;
                let restore = mute_audio_session(binding, journal_path)?;
                Ok((AudioInput::Process(capture), Some(restore)))
            })(),
            DesiredInput::Media => media_audio
                .lock()
                .get(&source_id)
                .cloned()
                .map(|ring| (AudioInput::Media(ring), None))
                .ok_or_else(|| "media audio stream is not ready".to_string()),
        };
        match prepared {
            Ok((input, restore)) => {
                sources.insert(
                    source_id,
                    SourceCapture {
                        input,
                        gain: GainRamp::new(volume),
                        target_volume: volume,
                        muted,
                        restore,
                        journal_path: journal_path.clone(),
                    },
                );
                let _ = events.send(EngineEvent::SourceAvailable { source_id });
            }
            Err(reason) => {
                let _ = events.send(EngineEvent::SourceUnavailable { source_id, reason });
            }
        }
    }
}

fn capture_thread(
    process_id: u32,
    ring: Arc<Mutex<PcmRing>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|error| error.to_string())?;
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
    unsafe { client.SetEventHandle(event) }.map_err(|error| error.to_string())?;
    let capture: IAudioCaptureClient =
        unsafe { client.GetService() }.map_err(|error| error.to_string())?;
    unsafe { client.Start() }.map_err(|error| error.to_string())?;
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Acquire) {
        if unsafe { WaitForSingleObject(event, 100) } != WAIT_OBJECT_0 {
            continue;
        }
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
                for sample in samples.chunks_exact(2) {
                    output.push([
                        f32::from(sample[0]) / 32_768.0,
                        f32::from(sample[1]) / 32_768.0,
                    ]);
                }
            }
            unsafe { capture.ReleaseBuffer(frame_count) }.map_err(|error| error.to_string())?;
        }
    }
    let _ = unsafe { client.Stop() };
    let _ = unsafe { CloseHandle(event) };
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
