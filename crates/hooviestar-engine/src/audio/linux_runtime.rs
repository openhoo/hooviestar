//! Linux-AudioRuntime auf PipeWire.
//!
//! Verfolgt `Stream/Output/Audio`- und `Audio/Source`-Knoten über die Registry,
//! bindet `ApplicationAudio`-Quellen eindeutig über kanonischen Prozesspfad plus
//! Session-Gruppierungskennung, erzeugt je Quelle einen dynamischen Capture-Stream
//! in einen begrenzten [`PcmRing`] und mischt Anwendungs- und Medienringe mit
//! Gain-Rampe, Mute und Limiter in den 48-kHz-Stereo-F32LE-Programmstream.
//!
//! Echtzeitregeln: Die `process`-Callbacks allokieren nach dem Streamaufbau
//! keinen Heap und halten nur kurze Ring-/Rampen-Sperren; alle Mutationen an
//! Streams, Snapshot und Projekt laufen im Steuerthread außerhalb der
//! Echtzeit-Callbacks. Ereignisse (`SourceAvailable`, `SourceUnavailable`,
//! `Levels`, `AudioWarning`) entstehen ausschließlich aus tatsächlichen
//! Zählern und Zustandsübergängen.

use std::{
    cell::RefCell,
    collections::HashMap,
    io::Cursor,
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};
use pipewire::{
    self as pw,
    node::{Node, NodeListener},
    properties::properties,
    spa::{
        self,
        param::ParamType,
        param::audio::{AudioFormat, AudioInfoRaw},
        pod::{self, Pod},
        utils::{Direction, SpaTypes},
    },
    stream::{StreamBox, StreamFlags, StreamListener, StreamState},
    thread_loop::ThreadLoopRc,
    types::ObjectType,
};
use uuid::Uuid;

use crate::{
    audio::{GainRamp, LIMITER_CEILING, MediaAudioBus, PcmRing, SAMPLE_RATE},
    engine::{AudioWarningKind, EngineEvent, LevelEntry},
    project::{AudioSessionBinding, ProjectV1, Source},
};

/// Maximale Anzahl gemischter Quellen im Echtzeit-Mix; weitere bleiben ungemischt.
const MAX_MIX_INPUTS: usize = 16;
/// Kapazität je Quellring in Frames (100 ms bei 48 kHz).
const RING_CAPACITY_FRAMES: usize = 4_800;
/// Taktpause des Steuerthreads für Projektabgleich, Pegel und Warnungen.
const CONTROL_TICK: Duration = Duration::from_millis(100);
/// Gain-Rampe läuft auf höchstens ~10 ms an, um Klicks zu vermeiden.
const MAX_RAMP_FRAMES: u32 = 480;
/// Nach drei fehlgeschlagenen Bindungsversuchen wird erst nach dieser Tick-Anzahl erneut versucht.
const RETRY_TICKS_AFTER_FAILURES: u32 = 10;

pub struct AudioRuntime {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl AudioRuntime {
    pub fn start(
        project: Arc<RwLock<ProjectV1>>,
        events: mpsc::Sender<EngineEvent>,
        media_audio: MediaAudioBus,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("pipewire-mixer".into())
            .spawn(move || {
                if let Err(error) = run_pipewire(
                    project,
                    events.clone(),
                    media_audio,
                    thread_stop,
                    ready_sender,
                ) {
                    let _ = events.send(EngineEvent::EngineError {
                        message: format!("PipeWire-Ausgabe beendet: {error}"),
                    });
                }
            })
            .map_err(|error| error.to_string())?;
        match ready_receiver.recv_timeout(Duration::from_secs(10)) {
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

/// Ein von der Registry gemeldeter, bindbarer Audio-Knoten.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TrackedNode {
    id: u32,
    process_path: String,
    grouping_id: String,
}

/// Ergebnis der eindeutigen Bindungsauflösung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindResult {
    Bound(u32),
    Offline,
    Ambiguous(usize),
}

/// Kanonisiert einen Prozesspfad best-effort; ohne Auflösung bleibt die Eingabe.
fn canonical_process_path(raw: &str) -> String {
    Path::new(raw)
        .canonicalize()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

/// Gruppierungskette identisch zu `discovery::linux`: node.group, node.name,
/// application.process.session-id, sonst "pipewire".
fn grouping_from_props(
    node_group: Option<&str>,
    node_name: Option<&str>,
    session_id: Option<&str>,
) -> String {
    node_group
        .or(node_name)
        .or(session_id)
        .unwrap_or("pipewire")
        .to_string()
}

/// Baut aus Knoten-Eigenschaften einen bindbaren Knoten oder verwirft ihn.
///
/// Akzeptiert dieselben `media.class`-Werte wie `discovery::linux`
/// (`Stream/Output/Audio` und `Audio/Source`), damit jede angebotene Auswahl
/// auch gebunden werden kann. Der Prozesspfad wird bevorzugt über
/// `/proc/<pid>/exe` kanonisch aufgelöst und fällt auf
/// `application.process.binary` zurück.
fn tracked_from_props(id: u32, get: &dyn Fn(&str) -> Option<String>) -> Option<TrackedNode> {
    let media_class = get("media.class")?;
    if media_class != "Stream/Output/Audio" && media_class != "Audio/Source" {
        return None;
    }
    if get("application.name").as_deref() == Some("Hooviestar")
        || get("node.name").is_some_and(|name| name.starts_with("hooviestar"))
    {
        return None;
    }
    let process_id = get("application.process.id").and_then(|value| value.parse::<u32>().ok());
    let raw_path = process_id
        .and_then(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).ok())
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| get("application.process.binary"))?;
    Some(TrackedNode {
        id,
        process_path: canonical_process_path(&raw_path),
        grouping_id: grouping_from_props(
            get("node.group").as_deref(),
            get("node.name").as_deref(),
            get("application.process.session-id").as_deref(),
        ),
    })
}

/// Bindet nur bei genau einem Treffer; niemals still an den ersten Treffer.
fn resolve_binding(binding: &AudioSessionBinding, nodes: &[TrackedNode]) -> BindResult {
    let wanted_path = canonical_process_path(&binding.process_path);
    let matches: Vec<&TrackedNode> = nodes
        .iter()
        .filter(|node| {
            node.process_path == wanted_path && node.grouping_id == binding.session_grouping_id
        })
        .collect();
    match matches.as_slice() {
        [] => BindResult::Offline,
        [only] => BindResult::Bound(only.id),
        more => BindResult::Ambiguous(more.len()),
    }
}

/// Aushandeldes Capture-/Ausgabeformat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NegotiatedFormat {
    format: AudioFormat,
    rate: u32,
    channels: u32,
}

impl NegotiatedFormat {
    const PROGRAM: Self = Self {
        format: AudioFormat::F32LE,
        rate: SAMPLE_RATE,
        channels: 2,
    };
}

/// Prüft das Programmformat 48 kHz Stereo F32LE.
fn is_program_compatible(format: NegotiatedFormat) -> bool {
    format.format.as_raw() == NegotiatedFormat::PROGRAM.format.as_raw()
        && format.rate == NegotiatedProgramLayout::RATE
        && format.channels == NegotiatedProgramLayout::CHANNELS
}

/// Zentrale Konstanten des Programmformats.
struct NegotiatedProgramLayout;

impl NegotiatedProgramLayout {
    const RATE: u32 = SAMPLE_RATE;
    const CHANNELS: u32 = 2;
}

/// Parst einen aushandelten SPA-Format-Pod in sein Layout.
fn parse_negotiated_format(pod: &Pod) -> Option<NegotiatedFormat> {
    let mut info = AudioInfoRaw::new();
    info.parse(pod).ok()?;
    Some(NegotiatedFormat {
        format: info.format(),
        rate: info.rate(),
        channels: info.channels(),
    })
}

/// Erzeugt den EnumFormat-Pod für Programm- und Capture-Streams.
fn enum_format_params() -> Result<Vec<u8>, String> {
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(2);
    let object = spa::pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values = pod::serialize::PodSerializer::serialize(
        Cursor::new(Vec::new()),
        &pod::Value::Object(object),
    )
    .map_err(|error| error.to_string())?
    .0
    .into_inner();
    Ok(values)
}

/// Eine Live-Pegel-Meldung des Steuerthreads.
struct CounterWarning {
    kind: AudioWarningKind,
    message: String,
}

/// Vergleicht Ringzähler gegen die zuletzt gemeldeten Stände und liefert
/// Warnungen nur für tatsächliche Differenzen.
fn counter_warnings(
    last: &mut HashMap<Uuid, (u64, u64)>,
    inputs: &[Arc<MixInput>],
) -> Vec<CounterWarning> {
    let mut warnings = Vec::new();
    for input in inputs {
        let (overruns, underruns) = {
            let ring = input.ring.lock();
            (ring.overruns(), ring.underruns())
        };
        let entry = last.entry(input.source_id).or_insert((0, 0));
        let new_overruns = overruns.saturating_sub(entry.0);
        let new_underruns = underruns.saturating_sub(entry.1);
        *entry = (overruns, underruns);
        if new_overruns > 0 {
            warnings.push(CounterWarning {
                kind: AudioWarningKind::Overrun,
                message: format!(
                    "Tonpuffer-Überlauf für Quelle „{}“ ({new_overruns} Frames verworfen)",
                    input.name
                ),
            });
        }
        if new_underruns > 0 {
            warnings.push(CounterWarning {
                kind: AudioWarningKind::Underrun,
                message: format!(
                    "Tonpuffer-Unterlauf für Quelle „{}“ ({new_underruns} Frames still)",
                    input.name
                ),
            });
        }
    }
    warnings
}

/// Eine Quelle im Echtzeit-Mix.
///
/// `volume_bits`/`muted` schreibt allein der Steuerthread; die Rampen-Sperre
/// hält ausschließlich der Render-Callback. Pegelzähler schreiben nur
/// Echtzeit-Callbacks, lesen entleerend nur der Steuerthread.
struct MixInput {
    source_id: Uuid,
    name: String,
    ring: Arc<Mutex<PcmRing>>,
    volume_bits: AtomicU32,
    muted: AtomicBool,
    ramp: Mutex<GainRamp>,
    peak_bits: AtomicU32,
    square_sum_bits: AtomicU64,
    frames: AtomicU64,
}

impl MixInput {
    fn new(
        source_id: Uuid,
        name: String,
        ring: Arc<Mutex<PcmRing>>,
        volume: f32,
        muted: bool,
    ) -> Self {
        Self {
            source_id,
            name,
            ring,
            volume_bits: AtomicU32::new(volume.clamp(0.0, 1.0).to_bits()),
            muted: AtomicBool::new(muted),
            ramp: Mutex::new(GainRamp::new(volume.clamp(0.0, 1.0))),
            peak_bits: AtomicU32::new(0.0f32.to_bits()),
            square_sum_bits: AtomicU64::new(0.0f64.to_bits()),
            frames: AtomicU64::new(0),
        }
    }

    fn set_level(&self, volume: f32, muted: bool) {
        self.volume_bits
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Veröffentlicht Blockpegel aus dem Echtzeit-Callback ohne Sperren.
    fn publish_sample(&self, left: f32, right: f32) {
        // Positive Floats ordnen bitweise monoton, daher genügt ein CAS-Maximum.
        let peak_bits = left.abs().max(right.abs()).to_bits();
        let mut current = self.peak_bits.load(Ordering::Relaxed);
        while peak_bits > current {
            match self.peak_bits.compare_exchange_weak(
                current,
                peak_bits,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        let add = (f64::from(left * left + right * right) * 0.5).to_bits();
        let mut current = self.square_sum_bits.load(Ordering::Relaxed);
        loop {
            let sum = f64::from_bits(current) + f64::from_bits(add);
            match self.square_sum_bits.compare_exchange_weak(
                current,
                sum.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        self.frames.fetch_add(1, Ordering::Relaxed);
    }

    /// Entleert die Pegelzähler in eine LevelEntry.
    fn drain_level(&self) -> LevelEntry {
        let frames = self.frames.swap(0, Ordering::Relaxed);
        let peak = f32::from_bits(self.peak_bits.swap(0.0f32.to_bits(), Ordering::Relaxed));
        let squares = f64::from_bits(
            self.square_sum_bits
                .swap(0.0f64.to_bits(), Ordering::Relaxed),
        );
        LevelEntry {
            source_id: self.source_id,
            peak,
            rms: if frames == 0 {
                0.0
            } else {
                (squares / frames as f64).sqrt() as f32
            },
        }
    }
}

/// Momentaufnahme der Mixeingaben; im Echtzeit-Callback wird nur der
/// `Arc` geklont, nie ein Heap-Objekt erzeugt.
type MixSnapshot = Arc<[Arc<MixInput>]>;

/// Nutzerdaten des Capture-Realtime-Callbacks.
struct CaptureUserData {
    negotiated: AtomicBool,
}

/// Meldungen der Threadloop-Callbacks an den Steuerthread.
enum Notice {
    Global(TrackedNode),
    Removed(u32),
    CaptureReady(Uuid),
    CaptureFormatRejected { source_id: Uuid, reason: String },
    CaptureFailed { source_id: Uuid, reason: String },
}

struct BoundNode {
    id: u32,
    _listener: NodeListener,
    _node: Node,
}

/// Aktive Anwendungsaufnahme einer Quelle.
struct ActiveCapture<'c> {
    source_id: Uuid,
    node_id: u32,
    binding: AudioSessionBinding,
    // Droppt vor dem Stream, damit der Listener nie auf einen freigegebenen
    // Stream zeigt.
    _listener: StreamListener<CaptureUserData>,
    stream: StreamBox<'c>,
}

fn run_pipewire(
    project: Arc<RwLock<ProjectV1>>,
    events: mpsc::Sender<EngineEvent>,
    media_audio: MediaAudioBus,
    stop: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();
    // SAFETY: pw ist durch pw::init initialisiert; pw_thread_loop_new legt nur
    // eine neue Schleife samt Thread an.
    let thread_loop = unsafe { ThreadLoopRc::new(Some("hooviestar-audio"), None) }
        .map_err(|error| error.to_string())?;
    thread_loop.start();
    let setup_lock = thread_loop.lock();
    let context =
        pw::context::ContextRc::new(&thread_loop, None).map_err(|error| error.to_string())?;
    let core = context
        .connect_rc(None)
        .map_err(|error| error.to_string())?;
    let registry = core.get_registry_rc().map_err(|error| error.to_string())?;

    let (notice_sender, notice_receiver) = mpsc::channel::<Notice>();

    // Registry-Verfolgung: vollständige NodeInfo-Properties enthalten bei
    // PipeWire-Pulse erst Prozesspfad und PID; GlobalObject.props allein nicht.
    let bound_nodes = Rc::new(RefCell::new(Vec::<BoundNode>::new()));
    let registry_weak = registry.downgrade();
    let registry_listener = registry
        .add_listener_local()
        .global({
            let sender = notice_sender.clone();
            let nodes = bound_nodes.clone();
            move |global| {
                if global.type_ != ObjectType::Node {
                    return;
                }
                let Some(registry) = registry_weak.upgrade() else {
                    return;
                };
                let Ok(node): Result<Node, _> = registry.bind(global) else {
                    return;
                };
                let node_sender = sender.clone();
                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        let Some(props) = info.props() else {
                            return;
                        };
                        if let Some(node) =
                            tracked_from_props(info.id(), &|key| props.get(key).map(str::to_owned))
                        {
                            let _ = node_sender.send(Notice::Global(node));
                        }
                    })
                    .register();
                nodes.borrow_mut().push(BoundNode {
                    id: global.id,
                    _listener: listener,
                    _node: node,
                });
            }
        })
        .global_remove({
            let sender = notice_sender.clone();
            let nodes = bound_nodes.clone();
            move |id| {
                nodes.borrow_mut().retain(|node| node.id != id);
                let _ = sender.send(Notice::Removed(id));
            }
        })
        .register();

    // Gemeinsamer Mix-Snapshot zwischen Steuerthread und Render-Callback.
    let mix_inputs: Arc<RwLock<MixSnapshot>> =
        Arc::new(RwLock::new(Arc::<[Arc<MixInput>]>::from(Vec::new())));

    // Programmausgabe: 48 kHz Stereo F32LE Richtung Standard-Senke.
    let program_stream = StreamBox::new(
        &core,
        "Hooviestar Program",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_ROLE => "Game",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::AUDIO_CHANNELS => "2",
            *pw::keys::APP_NAME => "Hooviestar",
        },
    )
    .map_err(|error| error.to_string())?;
    let program_listener = program_stream
        .add_local_listener_with_user_data(())
        .process({
            let mix_inputs = mix_inputs.clone();
            move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let Some(bytes) = data.data() else {
                    return;
                };
                let frame_count = bytes.len() / 8;
                let snapshot = mix_inputs.read().clone();
                // Pro Block jede Quelle höchstens einmal sperren; nie pro Sample.
                let mut rings: [Option<parking_lot::MutexGuard<'_, PcmRing>>; MAX_MIX_INPUTS] =
                    std::array::from_fn(|_| None);
                let mut ramps: [Option<parking_lot::MutexGuard<'_, GainRamp>>; MAX_MIX_INPUTS] =
                    std::array::from_fn(|_| None);
                for slot in 0..snapshot.len().min(MAX_MIX_INPUTS) {
                    let input = &snapshot[slot];
                    let target = f32::from_bits(input.volume_bits.load(Ordering::Relaxed));
                    let mut ramp = input.ramp.lock();
                    ramp.set(target, MAX_RAMP_FRAMES.min(frame_count as u32));
                    ramps[slot] = Some(ramp);
                    rings[slot] = Some(input.ring.lock());
                }
                for frame_index in 0..frame_count {
                    let mut mixed = [0.0f32; 2];
                    for (slot, input) in snapshot.iter().enumerate().take(MAX_MIX_INPUTS) {
                        let sample = rings[slot].as_mut().map_or([0.0, 0.0], |ring| ring.pop());
                        let gain = if input.muted.load(Ordering::Relaxed) {
                            0.0
                        } else if let Some(ramp) = ramps[slot].as_mut() {
                            ramp.next_gain()
                        } else {
                            continue;
                        };
                        let left = sample[0] * gain;
                        let right = sample[1] * gain;
                        mixed[0] += left;
                        mixed[1] += right;
                        input.publish_sample(left, right);
                    }
                    let peak = mixed[0].abs().max(mixed[1].abs());
                    let limiter = if peak > LIMITER_CEILING {
                        LIMITER_CEILING / peak
                    } else {
                        1.0
                    };
                    let offset = frame_index * 8;
                    bytes[offset..offset + 4].copy_from_slice(&(mixed[0] * limiter).to_le_bytes());
                    bytes[offset + 4..offset + 8]
                        .copy_from_slice(&(mixed[1] * limiter).to_le_bytes());
                }
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = 8;
                *chunk.size_mut() = (frame_count * 8) as u32;
            }
        })
        .register()
        .map_err(|error| error.to_string())?;

    let values = enum_format_params()?;
    let mut params = [Pod::from_bytes(&values).ok_or_else(|| "invalid SPA audio pod".to_string())?];
    program_stream
        .connect(
            Direction::Output,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|error| error.to_string())?;
    drop(setup_lock);

    let _ = ready.send(Ok(()));

    // Steuerthread-Zustand.
    let mut nodes: HashMap<u32, TrackedNode> = HashMap::new();
    let mut captures: Vec<ActiveCapture<'_>> = Vec::new();
    let mut mixers: HashMap<Uuid, Arc<MixInput>> = HashMap::new();
    let mut availability: HashMap<Uuid, bool> = HashMap::new();
    let mut failures: HashMap<Uuid, u32> = HashMap::new();
    let mut retry_cooldown: HashMap<Uuid, u32> = HashMap::new();
    let mut last_counters: HashMap<Uuid, (u64, u64)> = HashMap::new();
    let mut last_tick = Instant::now();

    while !stop.load(Ordering::Acquire) {
        match notice_receiver.recv_timeout(CONTROL_TICK) {
            Ok(notice) => match notice {
                Notice::Global(node) => {
                    nodes.insert(node.id, node);
                }
                Notice::Removed(id) => {
                    nodes.remove(&id);
                }
                Notice::CaptureReady(source_id) => {
                    emit_availability(&events, &mut availability, source_id, true, "");
                }
                Notice::CaptureFormatRejected { source_id, reason } => {
                    emit_availability(&events, &mut availability, source_id, false, &reason);
                }
                Notice::CaptureFailed { source_id, reason } => {
                    {
                        let _loop_lock = thread_loop.lock();
                        captures.retain(|capture| capture.source_id != source_id);
                    }
                    mixers.remove(&source_id);
                    rebuild_snapshot(&mixers, &mix_inputs);
                    emit_availability(
                        &events,
                        &mut availability,
                        source_id,
                        false,
                        &format!("PipeWire-Capture fehlgeschlagen: {reason}"),
                    );
                    let _ = events.send(EngineEvent::AudioWarning {
                        kind: AudioWarningKind::DeviceInvalidated,
                        message: format!("PipeWire-Audio-Quelle ausgefallen: {reason}"),
                    });
                    *failures.entry(source_id).or_default() += 1;
                    retry_cooldown.insert(source_id, RETRY_TICKS_AFTER_FAILURES);
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if !stop.load(Ordering::Acquire) && last_tick.elapsed() >= CONTROL_TICK {
            last_tick = Instant::now();

            // 1) Projekt lesen und Sollzustand bilden.
            let project_guard = project.read();
            let desired_app: Vec<(Uuid, String, AudioSessionBinding, f32, bool)> = project_guard
                .sources
                .iter()
                .filter_map(|source| match source {
                    Source::ApplicationAudio {
                        id,
                        name,
                        binding,
                        volume,
                        muted,
                    } => Some((*id, name.clone(), binding.clone(), *volume, *muted)),
                    _ => None,
                })
                .collect();
            let desired_media: Vec<(Uuid, String, f32, bool)> = project_guard
                .sources
                .iter()
                .filter_map(|source| match source {
                    Source::Media {
                        id,
                        name,
                        volume,
                        muted,
                        ..
                    } => Some((*id, name.clone(), *volume, *muted)),
                    _ => None,
                })
                .collect();
            drop(project_guard);

            // 2) Entfernte Quellen abbauen.
            let removed_app: Vec<Uuid> = captures
                .iter()
                .map(|capture| capture.source_id)
                .chain(mixers.keys().copied())
                .filter(|id| {
                    !desired_app.iter().any(|(wanted, ..)| wanted == id)
                        && !desired_media.iter().any(|(wanted, ..)| wanted == id)
                })
                .collect();
            for source_id in removed_app {
                {
                    let _loop_lock = thread_loop.lock();
                    captures.retain(|capture| capture.source_id != source_id);
                }
                mixers.remove(&source_id);
                failures.remove(&source_id);
                retry_cooldown.remove(&source_id);
                last_counters.remove(&source_id);
                emit_availability(
                    &events,
                    &mut availability,
                    source_id,
                    false,
                    "Quelle wurde aus dem Projekt entfernt",
                );
                availability.remove(&source_id);
            }

            // 3) Anwendungsquellen synchronisieren.
            for (source_id, name, binding, volume, muted) in &desired_app {
                if let Some(capture) = captures
                    .iter()
                    .find(|capture| capture.source_id == *source_id)
                {
                    let node_matches_binding = nodes
                        .get(&capture.node_id)
                        .map(|node| {
                            node.process_path == canonical_process_path(&binding.process_path)
                                && node.grouping_id == binding.session_grouping_id
                        })
                        .unwrap_or(false);
                    if node_matches_binding && capture.binding == *binding {
                        if let Some(input) = mixers.get(source_id) {
                            input.set_level(*volume, *muted);
                        }
                        continue;
                    }
                    // Knoten verschwunden: Aufnahme lösen und neu binden.
                    {
                        let _loop_lock = thread_loop.lock();
                        captures.retain(|capture| capture.source_id != *source_id);
                    }
                    mixers.remove(source_id);
                    rebuild_snapshot(&mixers, &mix_inputs);
                    emit_availability(
                        &events,
                        &mut availability,
                        *source_id,
                        false,
                        "PipeWire-Knoten wurde entfernt",
                    );
                }

                if let Some(remaining) = retry_cooldown.get(source_id).copied() {
                    if remaining > 0 {
                        retry_cooldown.insert(*source_id, remaining - 1);
                        continue;
                    }
                    retry_cooldown.remove(source_id);
                }

                match resolve_binding(binding, &nodes.values().cloned().collect::<Vec<_>>()) {
                    BindResult::Bound(node_id) => {
                        let attempts = failures.get(source_id).copied().unwrap_or(0);
                        let ring = Arc::new(Mutex::new(PcmRing::new(RING_CAPACITY_FRAMES)));
                        let input = Arc::new(MixInput::new(
                            *source_id,
                            name.clone(),
                            ring.clone(),
                            *volume,
                            *muted,
                        ));
                        let stream_pair = {
                            let _loop_lock = thread_loop.lock();
                            connect_capture_stream(
                                &core,
                                node_id,
                                *source_id,
                                name,
                                ring,
                                notice_sender.clone(),
                            )
                        };
                        match stream_pair {
                            Ok(stream_pair) => {
                                captures.push(ActiveCapture {
                                    source_id: *source_id,
                                    node_id,
                                    binding: binding.clone(),
                                    stream: stream_pair.0,
                                    _listener: stream_pair.1,
                                });
                                mixers.insert(*source_id, input);
                                rebuild_snapshot(&mixers, &mix_inputs);
                                failures.remove(source_id);
                                retry_cooldown.remove(source_id);
                                // SourceAvailable folgt erst nach erfolgreicher
                                // Formataushandlung über CaptureReady.
                            }
                            Err(reason) => {
                                *failures.entry(*source_id).or_default() += 1;
                                if attempts + 1 >= 3 {
                                    retry_cooldown.insert(*source_id, RETRY_TICKS_AFTER_FAILURES);
                                }
                                emit_availability(
                                    &events,
                                    &mut availability,
                                    *source_id,
                                    false,
                                    &format!("PipeWire-Capture konnte nicht starten: {reason}"),
                                );
                            }
                        }
                    }
                    BindResult::Offline => {
                        emit_availability(
                            &events,
                            &mut availability,
                            *source_id,
                            false,
                            &format!(
                                "Keine eindeutige PipeWire-Audio-Quelle für „{}“",
                                binding.process_path
                            ),
                        );
                    }
                    BindResult::Ambiguous(count) => {
                        emit_availability(
                            &events,
                            &mut availability,
                            *source_id,
                            false,
                            &format!(
                                "Mehrdeutige PipeWire-Audio-Quelle für „{}“ ({count} Treffer)",
                                binding.process_path
                            ),
                        );
                    }
                }
            }

            // 4) Medienquellen mit Bus-Ringen verbinden.
            for (source_id, name, volume, muted) in &desired_media {
                if let Some(input) = mixers.get(source_id) {
                    input.set_level(*volume, *muted);
                    // Name wird beim Erzeugen für RT-freie Warntexte übernommen.
                    emit_availability(&events, &mut availability, *source_id, true, "");
                    continue;
                }
                let bus_ring = media_audio.lock().get(source_id).cloned();
                match bus_ring {
                    Some(ring) => {
                        mixers.insert(
                            *source_id,
                            Arc::new(MixInput::new(
                                *source_id,
                                name.clone(),
                                ring,
                                *volume,
                                *muted,
                            )),
                        );
                        rebuild_snapshot(&mixers, &mix_inputs);
                        emit_availability(&events, &mut availability, *source_id, true, "");
                    }
                    None => {
                        emit_availability(
                            &events,
                            &mut availability,
                            *source_id,
                            false,
                            "Medienquelle liefert noch keinen PCM-Datenstrom",
                        );
                    }
                }
            }

            // 5) Pegel melden und Pufferwarnungen aus echten Zählern ableiten.
            let snapshot = mix_inputs.read().clone();
            let entries: Vec<LevelEntry> =
                snapshot.iter().map(|input| input.drain_level()).collect();
            let _ = events.send(EngineEvent::Levels { entries });
            for warning in counter_warnings(&mut last_counters, &snapshot) {
                let _ = events.send(EngineEvent::AudioWarning {
                    kind: warning.kind,
                    message: warning.message,
                });
            }
            drop(snapshot);
        }
    }

    // Teardown-Reihenfolge: Captures, Programmausgabe und Registry-Listener
    // fallen lassen, danach die Threadloop-Schleife anhalten; Core, Context
    // und Loop geben ihre Ressourcen beim Scope-Ende frei.
    {
        let _loop_lock = thread_loop.lock();
        for capture in &captures {
            let _ = capture.stream.set_active(false);
        }
        let _ = program_stream.set_active(false);
        captures.clear();
        drop(registry_listener);
        bound_nodes.borrow_mut().clear();
        drop(program_listener);
        drop(program_stream);
    }
    thread_loop.stop();
    Ok(())
}

/// Sendet Verfügbarkeitsereignisse nur bei echten Zustandsübergängen.
fn emit_availability(
    events: &mpsc::Sender<EngineEvent>,
    state: &mut HashMap<Uuid, bool>,
    source_id: Uuid,
    available: bool,
    reason: &str,
) {
    if state.get(&source_id) == Some(&available) {
        return;
    }
    state.insert(source_id, available);
    let event = if available {
        EngineEvent::SourceAvailable { source_id }
    } else {
        EngineEvent::SourceUnavailable {
            source_id,
            reason: reason.to_string(),
        }
    };
    let _ = events.send(event);
}

/// Ersetzt den Mix-Snapshot durch die aktuelle Eingabemenge.
fn rebuild_snapshot(mixers: &HashMap<Uuid, Arc<MixInput>>, target: &RwLock<MixSnapshot>) {
    let inputs: Vec<Arc<MixInput>> = mixers.values().take(MAX_MIX_INPUTS).cloned().collect();
    *target.write() = Arc::from(inputs);
}

/// Baut den Capture-Stream für einen gebundenen Knoten auf.
fn connect_capture_stream<'c>(
    core: &'c pipewire::core::Core,
    node_id: u32,
    source_id: Uuid,
    name: &str,
    ring: Arc<Mutex<PcmRing>>,
    notices: mpsc::Sender<Notice>,
) -> Result<(StreamBox<'c>, StreamListener<CaptureUserData>), String> {
    let stream = StreamBox::new(
        core,
        "Hooviestar Aufnahme",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::AUDIO_CHANNELS => "2",
            *pw::keys::APP_NAME => "Hooviestar",
            *pw::keys::NODE_NAME => format!("Hooviestar Aufnahme {name}"),
        },
    )
    .map_err(|error| error.to_string())?;
    let listener = stream
        .add_local_listener_with_user_data(CaptureUserData {
            negotiated: AtomicBool::new(false),
        })
        .state_changed({
            let notices = notices.clone();
            move |_, _, _, state| {
                if let StreamState::Error(reason) = state {
                    let _ = notices.send(Notice::CaptureFailed {
                        source_id,
                        reason: reason.clone(),
                    });
                }
            }
        })
        .param_changed({
            let notices = notices.clone();
            move |_, user_data, id, pod| {
                if id != ParamType::Format.as_raw() {
                    return;
                }
                let Some(pod) = pod else {
                    user_data.negotiated.store(false, Ordering::Release);
                    return;
                };
                let parsed = parse_negotiated_format(pod);
                match parsed {
                    Some(format) if is_program_compatible(format) => {
                        user_data.negotiated.store(true, Ordering::Release);
                        let _ = notices.send(Notice::CaptureReady(source_id));
                    }
                    Some(format) => {
                        user_data.negotiated.store(false, Ordering::Release);
                        let _ = notices.send(Notice::CaptureFormatRejected {
                            source_id,
                            reason: format!(
                                "Aushandlung liefert {} Hz / {} Kanäle statt 48 kHz Stereo F32LE",
                                format.rate, format.channels
                            ),
                        });
                    }
                    None => {}
                }
            }
        })
        .process({
            move |stream, user_data| {
                if !user_data.negotiated.load(Ordering::Acquire) {
                    return;
                }
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let chunk = data.chunk();
                let offset = chunk.offset() as usize;
                let size = chunk.size() as usize;
                let Some(bytes) = data.data() else {
                    return;
                };
                let end = offset.saturating_add(size).min(bytes.len());
                if offset >= end {
                    return;
                }
                let usable = (end - offset) - ((end - offset) % 8);
                if usable == 0 {
                    return;
                }
                // Ein Ring-Snapshot je Block; Überläufe zählt PcmRing selbst.
                let mut ring_guard = ring.lock();
                for chunk8 in bytes[offset..offset + usable].as_chunks::<8>().0 {
                    let left = f32::from_le_bytes(chunk8[0..4].try_into().expect("f32-Slice"));
                    let right = f32::from_le_bytes(chunk8[4..8].try_into().expect("f32-Slice"));
                    ring_guard.push([left, right]);
                }
            }
        })
        .register()
        .map_err(|error| error.to_string())?;
    let values = enum_format_params()?;
    let mut params = [Pod::from_bytes(&values).ok_or_else(|| "invalid SPA audio pod".to_string())?];
    stream
        .connect(
            Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|error| error.to_string())?;
    Ok((stream, listener))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(path: &str, group: &str) -> AudioSessionBinding {
        AudioSessionBinding {
            process_path: path.into(),
            session_grouping_id: group.into(),
        }
    }

    #[test]
    fn resolve_binding_requires_unique_match() {
        let nodes = vec![
            TrackedNode {
                id: 11,
                process_path: "/usr/bin/game".into(),
                grouping_id: "game".into(),
            },
            TrackedNode {
                id: 12,
                process_path: "/usr/bin/game".into(),
                grouping_id: "game".into(),
            },
            TrackedNode {
                id: 13,
                process_path: "/opt/other/bin".into(),
                grouping_id: "other".into(),
            },
        ];
        assert_eq!(
            resolve_binding(&binding("/usr/bin/game", "game"), &nodes),
            BindResult::Ambiguous(2)
        );
        assert_eq!(
            resolve_binding(&binding("/usr/bin/missing", "game"), &nodes),
            BindResult::Offline
        );
        assert_eq!(
            resolve_binding(&binding("/opt/other/bin", "other"), &nodes),
            BindResult::Bound(13)
        );
    }

    #[test]
    fn tracked_from_props_filters_classes_and_derives_grouping() {
        let props = |values: &[(&str, &str)]| {
            let map: HashMap<String, String> = values
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            move |key: &str| map.get(key).cloned()
        };
        let self_props = props(&[
            ("media.class", "Stream/Output/Audio"),
            ("application.name", "Hooviestar"),
            ("application.process.binary", "/usr/bin/hooviestar"),
            ("node.name", "hooviestar-program"),
        ]);
        assert!(tracked_from_props(3, &self_props).is_none());
        let video_props = props(&[("media.class", "Video/Source")]);
        assert!(tracked_from_props(1, &video_props).is_none());

        let audio_props = props(&[
            ("media.class", "Stream/Output/Audio"),
            ("application.process.binary", "/usr/bin/game"),
            ("node.name", "game-output"),
        ]);
        let tracked = tracked_from_props(2, &audio_props).expect("bindbar");
        assert_eq!(tracked.grouping_id, "game-output");
        assert_eq!(tracked.process_path, "/usr/bin/game");

        let grouped = props(&[
            ("media.class", "Audio/Source"),
            ("application.process.binary", "/usr/bin/call"),
            ("node.name", "call-input"),
            ("node.group", "group-7"),
        ]);
        let tracked = tracked_from_props(3, &grouped).expect("bindbar");
        assert_eq!(tracked.grouping_id, "group-7");
    }

    #[test]
    fn parse_negotiated_format_checks_program_layout() {
        let make_pod = |format: AudioFormat, rate: u32, channels: u32| {
            let mut info = AudioInfoRaw::new();
            info.set_format(format);
            info.set_rate(rate);
            info.set_channels(channels);
            let object = spa::pod::Object {
                type_: SpaTypes::ObjectParamFormat.as_raw(),
                id: ParamType::EnumFormat.as_raw(),
                properties: info.into(),
            };
            pod::serialize::PodSerializer::serialize(
                Cursor::new(Vec::new()),
                &pod::Value::Object(object),
            )
            .expect("Serialisierung")
            .0
            .into_inner()
        };

        let good = make_pod(AudioFormat::F32LE, SAMPLE_RATE, 2);
        let pod = Pod::from_bytes(&good).expect("gültiger Pod");
        let parsed = parse_negotiated_format(pod).expect("Format lesbar");
        assert!(is_program_compatible(parsed));

        let bad = make_pod(AudioFormat::S16LE, 44_100, 2);
        let pod = Pod::from_bytes(&bad).expect("gültiger Pod");
        let parsed = parse_negotiated_format(pod).expect("Format lesbar");
        assert!(!is_program_compatible(parsed));
    }

    #[test]
    fn mix_input_accumulates_and_drains_levels() {
        let ring = Arc::new(Mutex::new(PcmRing::new(RING_CAPACITY_FRAMES)));
        let id = Uuid::new_v4();
        let input = MixInput::new(id, "Test".into(), ring, 1.0, false);
        for _ in 0..100 {
            input.publish_sample(0.5, -0.25);
        }
        let entry = input.drain_level();
        assert_eq!(entry.source_id, id);
        assert!((entry.peak - 0.5).abs() < 1e-6);
        let expected_rms = (0.3125_f64 * 0.5).sqrt() as f32;
        assert!((entry.rms - expected_rms).abs() < 1e-5);
        let drained = input.drain_level();
        assert_eq!(drained.peak, 0.0);
        assert_eq!(drained.rms, 0.0);
    }

    #[test]
    fn counter_warnings_only_report_new_counts() {
        let ring = Arc::new(Mutex::new(PcmRing::new(2)));
        let id = Uuid::new_v4();
        let input = Arc::new(MixInput::new(id, "Spiel".into(), ring.clone(), 1.0, false));
        let mut last = HashMap::new();

        assert!(counter_warnings(&mut last, std::slice::from_ref(&input)).is_empty());

        for frame in 0..5u32 {
            ring.lock().push([frame as f32, 0.0]);
        }
        let warnings = counter_warnings(&mut last, std::slice::from_ref(&input));
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0].kind, AudioWarningKind::Overrun));

        assert!(counter_warnings(&mut last, std::slice::from_ref(&input)).is_empty());
    }
}
