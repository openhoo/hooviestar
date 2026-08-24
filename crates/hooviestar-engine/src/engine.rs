#[cfg(target_os = "linux")]
use crate::{
    audio::linux_runtime::AudioRuntime,
    video::{linux::PipeWirePortalLink, vulkan::RenderRuntime},
};
use crate::{
    audio::media_audio_bus,
    persistence::{PersistenceError, ProjectStore, default_project_path},
    project::{OutputConfig, ProjectV1, Scene, SceneItem, Source, Transform},
    video::{MediaControlBus, media_control_bus},
};
#[cfg(target_os = "windows")]
use crate::{audio::windows_runtime::AudioRuntime, video::windows::RenderRuntime};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender},
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeSurfaceKind {
    #[default]
    Unknown,
    Win32,
    Xlib,
    Wayland,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeSurfaces {
    pub studio: usize,
    pub program: usize,
    pub preview: usize,
    pub display: usize,
    pub kind: NativeSurfaceKind,
    pub program_width: u32,
    pub program_height: u32,
    pub preview_width: u32,
    pub preview_height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum EngineCommand {
    AddSource {
        source: Source,
    },
    RemoveSource {
        source_id: Uuid,
    },
    UpdateSource {
        source: Source,
    },
    AddScene {
        scene_id: Uuid,
        name: String,
    },
    RemoveScene {
        scene_id: Uuid,
    },
    RenameScene {
        scene_id: Uuid,
        name: String,
    },
    SetActiveScene {
        scene_id: Uuid,
    },
    SetSceneHotkey {
        scene_id: Uuid,
        hotkey: Option<String>,
    },
    AddSceneItem {
        scene_id: Uuid,
        item_id: Uuid,
        source_id: Uuid,
        transform: Transform,
    },
    RemoveSceneItem {
        scene_id: Uuid,
        item_id: Uuid,
    },
    SetItemVisible {
        scene_id: Uuid,
        item_id: Uuid,
        visible: bool,
    },
    SetItemLocked {
        scene_id: Uuid,
        item_id: Uuid,
        locked: bool,
    },
    ReorderSceneItem {
        scene_id: Uuid,
        item_id: Uuid,
        index: usize,
    },
    SetTransform {
        scene_id: Uuid,
        item_id: Uuid,
        transform: Transform,
    },
    SetOutputConfig {
        output: OutputConfig,
    },
    SetMediaPlaying {
        source_id: Uuid,
        playing: bool,
    },
    MediaSeek {
        source_id: Uuid,
        position_seconds: f64,
    },
    SetAudioVolume {
        source_id: Uuid,
        volume: f32,
    },
    SetAudioMuted {
        source_id: Uuid,
        muted: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRecoveryPhase {
    Started,
    Succeeded,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioWarningKind {
    Underrun,
    Overrun,
    DeviceInvalidated,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelEntry {
    pub source_id: Uuid,
    pub peak: f32,
    pub rms: f32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRuntimeState {
    pub playing: bool,
    pub position_seconds: f64,
    pub duration_seconds: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum EngineEvent {
    Snapshot {
        project: ProjectV1,
    },
    SourceAvailable {
        source_id: Uuid,
    },
    SourceUnavailable {
        source_id: Uuid,
        reason: String,
    },
    Levels {
        entries: Vec<LevelEntry>,
    },
    HotkeyError {
        scene_id: Uuid,
        message: String,
    },
    DeviceRecovery {
        phase: DeviceRecoveryPhase,
        detail: Option<String>,
    },
    MediaState {
        source_id: Uuid,
        state: MediaRuntimeState,
    },
    UnsupportedMedia {
        source_id: Uuid,
        reason: String,
    },
    AudioWarning {
        kind: AudioWarningKind,
        message: String,
    },
    EngineError {
        message: String,
    },
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("invalid project: {0}")]
    InvalidProject(String),
    #[error("not found: {0}")]
    NotFound(&'static str),
    #[error("engine event receiver already taken")]
    EventReceiverTaken,
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error("renderer startup failed: {0}")]
    Renderer(String),
}

pub struct EngineHandle {
    transition: Mutex<()>,
    project: Arc<RwLock<ProjectV1>>,
    events: Sender<EngineEvent>,
    receiver: Mutex<Option<Receiver<EngineEvent>>>,
    store: ProjectStore,
    #[cfg(target_os = "windows")]
    renderer: RenderRuntime,
    #[cfg(target_os = "linux")]
    renderer: Option<RenderRuntime>,
    #[cfg(target_os = "linux")]
    portal_link: Arc<PipeWirePortalLink>,
    #[cfg(target_os = "linux")]
    surfaces: Arc<RwLock<NativeSurfaces>>,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    audio: Option<AudioRuntime>,
    media_control: MediaControlBus,
}
impl EngineHandle {
    pub fn start(surfaces: NativeSurfaces, output: OutputConfig) -> Result<Self, EngineError> {
        let (events, receiver) = mpsc::channel();
        let path = default_project_path()?;
        let existed = path.exists();
        let (store, mut project, corrupt_backup) = ProjectStore::start(path)?;
        if !existed || corrupt_backup.is_some() {
            project.output = output;
            store.submit(project.clone())?;
            store.flush()?;
        }
        project.validate().map_err(EngineError::InvalidProject)?;
        let project = Arc::new(RwLock::new(project));
        let media_control = media_control_bus();
        let media_audio = media_audio_bus();
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let audio = match AudioRuntime::start(project.clone(), events.clone(), media_audio.clone())
        {
            Ok(audio) => Some(audio),
            Err(error) => {
                let _ = events.send(EngineEvent::EngineError {
                    message: format!("Anwendungs-Audio nicht verfügbar: {error}"),
                });
                None
            }
        };
        #[cfg(target_os = "windows")]
        let renderer = RenderRuntime::start(
            surfaces,
            project.clone(),
            events.clone(),
            media_audio.clone(),
            media_control.clone(),
        )
        .map_err(|error| EngineError::Renderer(error.to_string()))?;
        #[cfg(target_os = "linux")]
        let portal_link = Arc::new(PipeWirePortalLink::new());
        #[cfg(target_os = "linux")]
        let surface_state = Arc::new(RwLock::new(surfaces));
        #[cfg(target_os = "linux")]
        let renderer = match RenderRuntime::start(
            surfaces,
            project.clone(),
            events.clone(),
            portal_link.clone(),
            media_audio.clone(),
            media_control.clone(),
            surface_state.clone(),
        ) {
            Ok(renderer) => Some(renderer),
            Err(error) => {
                let _ = events.send(EngineEvent::DeviceRecovery {
                    phase: DeviceRecoveryPhase::Failed,
                    detail: Some(format!("Vulkan nicht verfügbar: {error}")),
                });
                None
            }
        };
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let _ = surfaces;
        let _ = events.send(EngineEvent::Snapshot {
            project: project.read().clone(),
        });
        if let Some(backup) = corrupt_backup {
            let _ = events.send(EngineEvent::EngineError {
                message: format!("Beschädigtes Projekt wurde gesichert: {}", backup.display()),
            });
        }
        Ok(Self {
            transition: Mutex::new(()),
            project,
            events,
            receiver: Mutex::new(Some(receiver)),
            store,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            renderer,
            #[cfg(target_os = "linux")]
            portal_link,
            #[cfg(target_os = "linux")]
            surfaces: surface_state,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            audio,
            media_control,
        })
    }
    #[cfg(target_os = "linux")]
    pub fn portal_link(&self) -> Arc<PipeWirePortalLink> {
        self.portal_link.clone()
    }

    #[cfg(target_os = "linux")]
    pub fn set_surface_size(&self, label: &str, width: u32, height: u32) {
        let mut surfaces = self.surfaces.write();
        match label {
            "program" => {
                surfaces.program_width = width.max(1);
                surfaces.program_height = height.max(1);
            }
            "preview" => {
                surfaces.preview_width = width.max(1);
                surfaces.preview_height = height.max(1);
            }
            _ => {}
        }
    }
    pub fn snapshot(&self) -> ProjectV1 {
        self.project.read().clone()
    }
    pub fn take_events(&self) -> Result<Receiver<EngineEvent>, EngineError> {
        self.receiver
            .lock()
            .take()
            .ok_or(EngineError::EventReceiverTaken)
    }
    pub fn command(&self, command: EngineCommand) -> Result<(), EngineError> {
        // Ein Linearisierungspunkt fuer Snapshot -> Apply -> Persistenz ->
        // Commit; sonst verlieren parallele Commands Updates auf demselben
        // Basis-Snapshot.
        let _transition = self.transition.lock();
        let media_playing = match &command {
            EngineCommand::SetMediaPlaying { source_id, playing } => Some((*source_id, *playing)),
            _ => None,
        };
        let media_seek = match &command {
            EngineCommand::MediaSeek {
                source_id,
                position_seconds,
            } => Some((*source_id, *position_seconds)),
            _ => None,
        };
        let removed_source = match &command {
            EngineCommand::RemoveSource { source_id } => Some(*source_id),
            _ => None,
        };
        if let Some((_, position_seconds)) = media_seek
            && (!position_seconds.is_finite() || position_seconds < 0.0)
        {
            return Err(EngineError::InvalidProject(
                "media seek position must be finite and non-negative".into(),
            ));
        }
        let media_target = media_playing
            .map(|(source_id, _)| source_id)
            .or(media_seek.map(|(source_id, _)| source_id));
        let mut next = self.snapshot();
        // Laufzeit-Steuerung trifft nur existierende Media-Quellen, sonst
        // sammeln sich Steuerungseintraege fuer unbekannte IDs an.
        if let Some(source_id) = media_target
            && !next
                .sources
                .iter()
                .any(|source| matches!(source, Source::Media { id, .. } if *id == source_id))
        {
            return Err(EngineError::NotFound("source"));
        }
        apply(&mut next, command)?;
        next.validate().map_err(EngineError::InvalidProject)?;
        self.store.submit(next.clone())?;
        *self.project.write() = next.clone();
        if let Some((source_id, playing)) = media_playing {
            self.media_control
                .write()
                .entry(source_id)
                .or_default()
                .playing = playing;
        }
        if let Some((source_id, position_seconds)) = media_seek {
            self.media_control
                .write()
                .entry(source_id)
                .or_default()
                .seek_seconds = Some(position_seconds);
        }
        if let Some(source_id) = removed_source {
            self.media_control.write().remove(&source_id);
        }
        let _ = self.events.send(EngineEvent::Snapshot { project: next });
        Ok(())
    }
    pub fn shutdown(&self) -> Result<(), EngineError> {
        let _transition = self.transition.lock();
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(audio) = &self.audio {
            audio.shutdown();
        }
        #[cfg(target_os = "windows")]
        self.renderer.shutdown();
        #[cfg(target_os = "linux")]
        if let Some(renderer) = &self.renderer {
            renderer.shutdown();
        }
        // Flush erst nach dem Teardown auswerten: Audio-Restore und Renderer-
        // Abbau dürfen nicht durch einen Persistenzfehler uebersprungen werden.
        let flush_result = self.store.flush();
        self.store.shutdown()?;
        flush_result.map_err(Into::into)
    }
}
fn scene_mut(project: &mut ProjectV1, id: Uuid) -> Result<&mut Scene, EngineError> {
    project
        .scenes
        .iter_mut()
        .find(|x| x.id == id)
        .ok_or(EngineError::NotFound("scene"))
}
fn apply(p: &mut ProjectV1, c: EngineCommand) -> Result<(), EngineError> {
    match c {
        EngineCommand::AddSource { source } => p.sources.push(source),
        EngineCommand::RemoveSource { source_id } => {
            p.sources.retain(|s| s.id() != source_id);
            for s in &mut p.scenes {
                s.items.retain(|i| i.source_id != source_id)
            }
        }
        EngineCommand::UpdateSource { source } => {
            let slot = p
                .sources
                .iter_mut()
                .find(|s| s.id() == source.id())
                .ok_or(EngineError::NotFound("source"))?;
            *slot = source
        }
        EngineCommand::AddScene { scene_id, name } => p.scenes.push(Scene {
            id: scene_id,
            name,
            hotkey: None,
            items: vec![],
        }),
        EngineCommand::RemoveScene { scene_id } => {
            if !p.scenes.iter().any(|scene| scene.id == scene_id) {
                return Err(EngineError::NotFound("scene"));
            }
            if p.scenes.len() == 1 {
                return Err(EngineError::InvalidProject(
                    "project requires a scene".into(),
                ));
            }
            p.scenes.retain(|scene| scene.id != scene_id);
            if p.active_scene_id == scene_id {
                p.active_scene_id = p.scenes[0].id
            }
        }
        EngineCommand::RenameScene { scene_id, name } => scene_mut(p, scene_id)?.name = name,
        EngineCommand::SetActiveScene { scene_id } => p.active_scene_id = scene_id,
        EngineCommand::SetSceneHotkey { scene_id, hotkey } => {
            scene_mut(p, scene_id)?.hotkey = hotkey
        }
        EngineCommand::AddSceneItem {
            scene_id,
            item_id,
            source_id,
            transform,
        } => scene_mut(p, scene_id)?.items.push(SceneItem {
            id: item_id,
            source_id,
            visible: true,
            locked: false,
            transform,
        }),
        EngineCommand::RemoveSceneItem { scene_id, item_id } => {
            scene_mut(p, scene_id)?.items.retain(|i| i.id != item_id)
        }
        EngineCommand::SetItemVisible {
            scene_id,
            item_id,
            visible,
        } => {
            scene_mut(p, scene_id)?
                .items
                .iter_mut()
                .find(|i| i.id == item_id)
                .ok_or(EngineError::NotFound("item"))?
                .visible = visible
        }
        EngineCommand::SetItemLocked {
            scene_id,
            item_id,
            locked,
        } => {
            scene_mut(p, scene_id)?
                .items
                .iter_mut()
                .find(|i| i.id == item_id)
                .ok_or(EngineError::NotFound("item"))?
                .locked = locked
        }
        EngineCommand::ReorderSceneItem {
            scene_id,
            item_id,
            index,
        } => {
            let items = &mut scene_mut(p, scene_id)?.items;
            let old = items
                .iter()
                .position(|i| i.id == item_id)
                .ok_or(EngineError::NotFound("item"))?;
            let item = items.remove(old);
            let at = index.min(items.len());
            items.insert(at, item)
        }
        EngineCommand::SetTransform {
            scene_id,
            item_id,
            transform,
        } => {
            scene_mut(p, scene_id)?
                .items
                .iter_mut()
                .find(|i| i.id == item_id)
                .ok_or(EngineError::NotFound("item"))?
                .transform = transform
        }
        EngineCommand::SetOutputConfig { output } => p.output = output,
        EngineCommand::SetMediaPlaying { .. } | EngineCommand::MediaSeek { .. } => {}
        EngineCommand::SetAudioVolume { source_id, volume } => {
            let source = p
                .sources
                .iter_mut()
                .find(|s| s.id() == source_id)
                .ok_or(EngineError::NotFound("source"))?;
            match source {
                Source::Media { volume: v, .. } | Source::ApplicationAudio { volume: v, .. } => {
                    *v = volume.clamp(0.0, 1.0)
                }
                _ => return Err(EngineError::InvalidProject("source has no audio".into())),
            }
        }
        EngineCommand::SetAudioMuted { source_id, muted } => {
            let source = p
                .sources
                .iter_mut()
                .find(|s| s.id() == source_id)
                .ok_or(EngineError::NotFound("source"))?;
            match source {
                Source::Media { muted: v, .. } | Source::ApplicationAudio { muted: v, .. } => {
                    *v = muted
                }
                _ => return Err(EngineError::InvalidProject("source has no audio".into())),
            }
        }
    }
    Ok(())
}
