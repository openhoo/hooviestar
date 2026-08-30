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
    /// Windows: initiale Kindfenster-Größe, vom Renderer nicht gelesen;
    /// Linux: aktuelle Preview-Größe, vom Vulkan-Renderer konsumiert.
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
    /// Entfernt die Quelle und kaskadierend alle referenzierenden Szenen-
    /// Items. Die Sperre (SceneItem.locked) schuetzt die Platzierung eines
    /// Items, nicht seine Existenz: Auch gesperrte Items werden hier
    /// mitentfernt, damit keine verwaisten Referenzen zurueckbleiben.
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
    AudioRecovered,
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
        let updated_source = match &command {
            EngineCommand::UpdateSource { source } => Some(source.id()),
            _ => None,
        };
        if let Some((_, position_seconds)) = media_seek {
            validate_seek_position(position_seconds)?;
        }
        let media_target = media_playing
            .map(|(source_id, _)| source_id)
            .or(media_seek.map(|(source_id, _)| source_id));
        // Laufzeit-Steuerung trifft nur existierende Media-Quellen, sonst
        // sammeln sich Steuerungseintraege fuer unbekannte IDs an.
        if let Some(source_id) = media_target {
            require_media_source(&self.project.read(), source_id)?;
            // Play/Pause und Seek mutieren das Projekt nie; der Zustand liegt
            // allein im media_control-Bus. Deshalb frueh zurueck: kein
            // Snapshot-Klon, kein Persistenz-Submit, kein Snapshot-Event.
            if let Some((id, playing)) = media_playing {
                let mut bus = self.media_control.write();
                let entry = bus.entry(id).or_default();
                // Jeder Play erhoeht die Epoche: ausstehende
                // Rueckschreibvorgaenge des Render-Threads nach
                // fehlgeschlagenem Restart-Seek duerfen diesen
                // Wunsch nicht mehr ueberschreiben.
                if playing {
                    entry.epoch = entry.epoch.wrapping_add(1);
                }
                entry.playing = playing;
            }
            if let Some((id, position_seconds)) = media_seek {
                self.media_control
                    .write()
                    .entry(id)
                    .or_default()
                    .seek_seconds = Some(position_seconds);
            }
            return Ok(());
        }
        let mut next = self.snapshot();
        apply(&mut next, command)?;
        next.validate().map_err(EngineError::InvalidProject)?;
        self.store.submit(next.clone())?;
        *self.project.write() = next.clone();
        prune_media_control(
            &self.media_control,
            &self.project.read(),
            removed_source,
            updated_source,
        );
        let _ = self.events.send(EngineEvent::Snapshot { project: next });
        Ok(())
    }
    /// Teardown in fester Reihenfolge:
    /// 1) Audio-Runtime stoppen und Sitzungen wiederherstellen,
    /// 2) Renderer samt Swapchains abbauen,
    /// 3) Projekt-Writer beenden inklusive finalem Save - dessen
    ///    Ergebnis ist das massgebliche Persistenzoutcome.
    ///
    /// Ein Persistenzfehler darf Audio-Restore und Renderer-Abbau
    /// nicht ueberspringen.
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
        // Der finale Save des Projekt-Writers ist das massgebliche
        // Persistenzoutcome; ein vorheriger Flush waere redundant.
        self.store.shutdown().map_err(Into::into)
    }
}
fn scene_mut(project: &mut ProjectV1, id: Uuid) -> Result<&mut Scene, EngineError> {
    project
        .scenes
        .iter_mut()
        .find(|x| x.id == id)
        .ok_or(EngineError::NotFound("scene"))
}

fn validate_seek_position(position_seconds: f64) -> Result<(), EngineError> {
    if !position_seconds.is_finite() || position_seconds < 0.0 {
        Err(EngineError::InvalidProject(
            "media seek position must be finite and non-negative".into(),
        ))
    } else {
        Ok(())
    }
}

fn require_media_source(project: &ProjectV1, source_id: Uuid) -> Result<(), EngineError> {
    if project
        .sources
        .iter()
        .any(|source| matches!(source, Source::Media { id, .. } if *id == source_id))
    {
        Ok(())
    } else {
        Err(EngineError::NotFound("source"))
    }
}

/// Entfernt verwaiste media_control-Eintraege nach projektveraendernden
/// Commands: RemoveSource nimmt den Eintrag der Quelle mit, ein Typwechsel
/// weg von Media hinterlaesst keinen Steuerungseintrag.
fn prune_media_control(
    media_control: &MediaControlBus,
    project: &ProjectV1,
    removed_source: Option<Uuid>,
    updated_source: Option<Uuid>,
) {
    if let Some(source_id) = removed_source {
        media_control.write().remove(&source_id);
    }
    if let Some(source_id) = updated_source {
        // Typwechsel weg von Media hinterlaesst keinen Steuerungseintrag.
        if require_media_source(project, source_id).is_err() {
            media_control.write().remove(&source_id);
        }
    }
}
fn apply(p: &mut ProjectV1, c: EngineCommand) -> Result<(), EngineError> {
    match c {
        EngineCommand::AddSource { source } => p.sources.push(source),
        EngineCommand::RemoveSource { source_id } => {
            if !p.sources.iter().any(|s| s.id() == source_id) {
                return Err(EngineError::NotFound("source"));
            }
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
        EngineCommand::SetActiveScene { scene_id } => {
            if !p.scenes.iter().any(|scene| scene.id == scene_id) {
                return Err(EngineError::NotFound("scene"));
            }
            p.active_scene_id = scene_id
        }
        EngineCommand::SetSceneHotkey { scene_id, hotkey } => {
            scene_mut(p, scene_id)?.hotkey = hotkey
        }
        EngineCommand::AddSceneItem {
            scene_id,
            item_id,
            source_id,
            transform,
        } => {
            let scene = scene_mut(p, scene_id)?;
            if scene.items.iter().any(|item| item.source_id == source_id) {
                return Err(EngineError::InvalidProject(
                    "source appears more than once in scene".into(),
                ));
            }
            scene.items.push(SceneItem {
                id: item_id,
                source_id,
                visible: true,
                locked: false,
                transform,
            });
        }
        EngineCommand::RemoveSceneItem { scene_id, item_id } => {
            let scene = scene_mut(p, scene_id)?;
            let Some(item) = scene.items.iter().find(|i| i.id == item_id) else {
                return Err(EngineError::NotFound("item"));
            };
            if item.locked {
                return Err(EngineError::InvalidProject("scene item locked".into()));
            }
            scene.items.retain(|i| i.id != item_id)
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
            if items[old].locked {
                return Err(EngineError::InvalidProject("scene item locked".into()));
            }
            let item = items.remove(old);
            let at = index.min(items.len());
            items.insert(at, item)
        }
        EngineCommand::SetTransform {
            scene_id,
            item_id,
            transform,
        } => {
            let item = scene_mut(p, scene_id)?
                .items
                .iter_mut()
                .find(|i| i.id == item_id)
                .ok_or(EngineError::NotFound("item"))?;
            if item.locked {
                return Err(EngineError::InvalidProject("scene item locked".into()));
            }
            item.transform = transform
        }
        EngineCommand::SetOutputConfig { output } => p.output = output,
        // Reine Laufzeitbefehle werden in command() vor apply() abgefertigt;
        // dieser Arm bleibt nur fuer die Match-Vollstaendigkeit bestehen.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::TextAlign;
    use crate::video::MediaControl;

    fn text_source(id: Uuid) -> Source {
        Source::Text {
            id,
            name: "Text".into(),
            text: "hallo".into(),
            font_family: "Sans".into(),
            font_size_px: 32.0,
            font_weight: 400,
            color: "#ffffff".into(),
            background_color: "#000000".into(),
            align: TextAlign::Left,
        }
    }

    fn media_source(id: Uuid) -> Source {
        Source::Media {
            id,
            name: "Media".into(),
            path: "/tmp/video.mp4".into(),
            looped: false,
            continue_when_hidden: false,
            restart_on_show: false,
            volume: 1.0,
            muted: false,
        }
    }

    fn add_item(project: &mut ProjectV1, scene_id: Uuid, item_id: Uuid, source_id: Uuid) {
        apply(
            project,
            EngineCommand::AddSceneItem {
                scene_id,
                item_id,
                source_id,
                transform: Transform::default(),
            },
        )
        .unwrap();
    }

    fn item(project: &ProjectV1, scene_id: Uuid, item_id: Uuid) -> &SceneItem {
        project
            .scenes
            .iter()
            .find(|scene| scene.id == scene_id)
            .unwrap()
            .items
            .iter()
            .find(|i| i.id == item_id)
            .unwrap()
    }

    #[test]
    fn reorder_scene_item_clamps_out_of_range_index_to_end() {
        let mut project = ProjectV1::empty();
        let scene = project.scenes[0].id;
        let source_ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let (first, middle, last) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for source_id in source_ids {
            apply(
                &mut project,
                EngineCommand::AddSource {
                    source: text_source(source_id),
                },
            )
            .unwrap();
        }
        add_item(&mut project, scene, first, source_ids[0]);
        add_item(&mut project, scene, middle, source_ids[1]);
        add_item(&mut project, scene, last, source_ids[2]);
        apply(
            &mut project,
            EngineCommand::ReorderSceneItem {
                scene_id: scene,
                item_id: middle,
                index: 99,
            },
        )
        .unwrap();
        let order: Vec<Uuid> = project.scenes[0].items.iter().map(|i| i.id).collect();
        assert_eq!(order, vec![first, last, middle]);
    }

    #[test]
    fn remove_scene_rejects_removing_last_scene() {
        let mut project = ProjectV1::empty();
        let scene_ids: Vec<Uuid> = project.scenes.iter().map(|scene| scene.id).collect();
        for scene_id in &scene_ids[..2] {
            apply(
                &mut project,
                EngineCommand::RemoveScene {
                    scene_id: *scene_id,
                },
            )
            .unwrap();
        }
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::RemoveScene {
                    scene_id: scene_ids[2]
                }
            ),
            Err(EngineError::InvalidProject(message))
                if message == "project requires a scene"
        ));
        assert_eq!(project.scenes.len(), 1);
    }

    #[test]
    fn removing_active_scene_repoints_active_scene_to_first_remaining() {
        let mut project = ProjectV1::empty();
        let removed = project.active_scene_id;
        apply(
            &mut project,
            EngineCommand::RemoveScene { scene_id: removed },
        )
        .unwrap();
        assert_eq!(project.scenes.len(), 2);
        assert_ne!(project.active_scene_id, removed);
        assert_eq!(project.active_scene_id, project.scenes[0].id);
    }

    #[test]
    fn media_runtime_commands_require_a_media_source() {
        let mut project = ProjectV1::empty();
        let text = Uuid::new_v4();
        let media = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: text_source(text),
            },
        )
        .unwrap();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: media_source(media),
            },
        )
        .unwrap();
        // command() leitet SetMediaPlaying/MediaSeek durch require_media_source;
        // ein vollstaendiger EngineHandle ist im Unit-Test nicht baubar.
        require_media_source(&project, media).unwrap();
        assert!(matches!(
            require_media_source(&project, text),
            Err(EngineError::NotFound("source"))
        ));
        assert!(matches!(
            require_media_source(&project, Uuid::new_v4()),
            Err(EngineError::NotFound("source"))
        ));
    }

    #[test]
    fn media_seek_rejects_negative_and_non_finite_positions() {
        assert!(validate_seek_position(-0.001).is_err());
        assert!(matches!(
            validate_seek_position(f64::NAN),
            Err(EngineError::InvalidProject(_))
        ));
        assert!(matches!(
            validate_seek_position(f64::NEG_INFINITY),
            Err(EngineError::InvalidProject(_))
        ));
        assert!(matches!(
            validate_seek_position(f64::INFINITY),
            Err(EngineError::InvalidProject(_))
        ));
        validate_seek_position(0.0).unwrap();
        validate_seek_position(12.5).unwrap();
    }

    #[test]
    fn remove_source_cascades_scene_items() {
        let mut project = ProjectV1::empty();
        let scene = project.scenes[0].id;
        let (kept_source, dropped_source) = (Uuid::new_v4(), Uuid::new_v4());
        let (dropped_item, kept_item) = (Uuid::new_v4(), Uuid::new_v4());
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: text_source(dropped_source),
            },
        )
        .unwrap();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: text_source(kept_source),
            },
        )
        .unwrap();
        add_item(&mut project, scene, dropped_item, dropped_source);
        add_item(&mut project, scene, kept_item, kept_source);
        apply(
            &mut project,
            EngineCommand::RemoveSource {
                source_id: dropped_source,
            },
        )
        .unwrap();
        assert!(!project.sources.iter().any(|s| s.id() == dropped_source));
        let remaining: Vec<Uuid> = project.scenes[0].items.iter().map(|i| i.id).collect();
        assert_eq!(remaining, vec![kept_item]);
    }

    #[test]
    fn remove_source_rejects_unknown_source_with_not_found() {
        let mut project = ProjectV1::empty();
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::RemoveSource {
                    source_id: Uuid::new_v4()
                }
            ),
            Err(EngineError::NotFound("source"))
        ));
        assert!(project.sources.is_empty());
    }

    #[test]
    fn locked_scene_item_rejects_remove_transform_and_reorder() {
        let mut project = ProjectV1::empty();
        let scene = project.scenes[0].id;
        let source_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: text_source(source_id),
            },
        )
        .unwrap();
        add_item(&mut project, scene, item_id, source_id);
        apply(
            &mut project,
            EngineCommand::SetItemLocked {
                scene_id: scene,
                item_id,
                locked: true,
            },
        )
        .unwrap();
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::RemoveSceneItem {
                    scene_id: scene,
                    item_id
                }
            ),
            Err(EngineError::InvalidProject(message)) if message == "scene item locked"
        ));
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::SetTransform {
                    scene_id: scene,
                    item_id,
                    transform: Transform {
                        width: 10.0,
                        ..Default::default()
                    }
                }
            ),
            Err(EngineError::InvalidProject(message)) if message == "scene item locked"
        ));
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::ReorderSceneItem {
                    scene_id: scene,
                    item_id,
                    index: 0
                }
            ),
            Err(EngineError::InvalidProject(message)) if message == "scene item locked"
        ));
        assert_eq!(
            item(&project, scene, item_id).transform,
            Transform::default()
        );
        assert_eq!(project.scenes[0].items.len(), 1);
    }

    #[test]
    fn add_scene_item_rejects_duplicate_source_without_mutation() {
        let mut project = ProjectV1::empty();
        let scene = project.scenes[0].id;
        let source_id = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: text_source(source_id),
            },
        )
        .unwrap();
        add_item(&mut project, scene, Uuid::new_v4(), source_id);
        let before = project.clone();
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::AddSceneItem {
                    scene_id: scene,
                    item_id: Uuid::new_v4(),
                    source_id,
                    transform: Transform::default(),
                }
            ),
            Err(EngineError::InvalidProject(message))
                if message == "source appears more than once in scene"
        ));
        assert_eq!(project, before);
    }

    #[test]
    fn locked_scene_item_still_toggles_visibility() {
        let mut project = ProjectV1::empty();
        let scene = project.scenes[0].id;
        let source_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: text_source(source_id),
            },
        )
        .unwrap();
        add_item(&mut project, scene, item_id, source_id);
        apply(
            &mut project,
            EngineCommand::SetItemLocked {
                scene_id: scene,
                item_id,
                locked: true,
            },
        )
        .unwrap();
        apply(
            &mut project,
            EngineCommand::SetItemVisible {
                scene_id: scene,
                item_id,
                visible: false,
            },
        )
        .unwrap();
        assert!(!item(&project, scene, item_id).visible);
    }

    #[test]
    fn remove_scene_item_rejects_unknown_item_with_not_found() {
        let mut project = ProjectV1::empty();
        let scene = project.scenes[0].id;
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::RemoveSceneItem {
                    scene_id: scene,
                    item_id: Uuid::new_v4(),
                }
            ),
            Err(EngineError::NotFound("item"))
        ));
        assert_eq!(project.scenes[0].items.len(), 0);
    }

    #[test]
    fn set_active_scene_rejects_unknown_scene_with_not_found() {
        let mut project = ProjectV1::empty();
        let initial = project.active_scene_id;
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::SetActiveScene {
                    scene_id: Uuid::new_v4(),
                }
            ),
            Err(EngineError::NotFound("scene"))
        ));
        // Fehlgeschlagene Zuweisung veraendert die aktive Szene nicht.
        assert_eq!(project.active_scene_id, initial);
    }

    #[test]
    fn update_source_away_from_media_drops_media_runtime_state() {
        let mut project = ProjectV1::empty();
        let media = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: media_source(media),
            },
        )
        .unwrap();
        // Laufzeitsteuerung zielt auf die Media-Quelle ...
        require_media_source(&project, media).unwrap();
        apply(
            &mut project,
            EngineCommand::UpdateSource {
                source: text_source(media),
            },
        )
        .unwrap();
        assert!(matches!(
            project.sources.iter().find(|s| s.id() == media),
            Some(Source::Text { .. })
        ));
        // Genau dieses Praedikat loest in command() das Entfernen des
        // media_control-Eintrags aus; ein vollstaendiger EngineHandle ist
        // im Unit-Test nicht baubar.
        assert!(require_media_source(&project, media).is_err());
    }

    #[test]
    fn prune_media_control_drops_only_removed_or_demediated_entries() {
        let mut project = ProjectV1::empty();
        let media = Uuid::new_v4();
        let text = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: media_source(media),
            },
        )
        .unwrap();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: text_source(text),
            },
        )
        .unwrap();
        // Projektvariante, in der die Media-Quelle auf Text umgeschaltet ist.
        let mut demoted = project.clone();
        apply(
            &mut demoted,
            EngineCommand::UpdateSource {
                source: text_source(media),
            },
        )
        .unwrap();
        let entry = MediaControl {
            playing: true,
            seek_seconds: Some(1.5),
            epoch: 0,
        };
        // Unveraenderter Zustand ohne Remove/Update: Eintrag bleibt bestehen.
        let bus = media_control_bus();
        bus.write().insert(media, entry);
        prune_media_control(&bus, &project, None, None);
        let control = bus.read().get(&media).copied().unwrap();
        assert!(control.playing);
        assert_eq!(control.seek_seconds, Some(1.5));
        // RemoveSource einer Media-Quelle entfernt ihren Eintrag.
        let bus = media_control_bus();
        bus.write().insert(media, entry);
        prune_media_control(&bus, &project, Some(media), None);
        assert!(bus.read().get(&media).is_none());
        // UpdateSource Media -> Text: genau dieser Typwechsel entfernt den Eintrag.
        let bus = media_control_bus();
        bus.write().insert(media, entry);
        prune_media_control(&bus, &demoted, None, Some(media));
        assert!(bus.read().get(&media).is_none());
        // UpdateSource Text -> Text beruehrt fremde Media-Eintraege nicht ...
        let bus = media_control_bus();
        bus.write().insert(media, entry);
        prune_media_control(&bus, &project, None, Some(text));
        let control = bus.read().get(&media).copied().unwrap();
        assert!(control.playing);
        assert_eq!(control.seek_seconds, Some(1.5));
        // ... und Media -> Media erhaelt den eigenen Eintrag ebenfalls.
        prune_media_control(&bus, &project, None, Some(media));
        let control = bus.read().get(&media).copied().unwrap();
        assert!(control.playing);
    }

    #[test]
    fn remove_scene_item_reports_locked_only_after_item_exists() {
        let mut project = ProjectV1::empty();
        let scene = project.scenes[0].id;
        let source_id = Uuid::new_v4();
        let item_id = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: text_source(source_id),
            },
        )
        .unwrap();
        add_item(&mut project, scene, item_id, source_id);
        apply(
            &mut project,
            EngineCommand::SetItemLocked {
                scene_id: scene,
                item_id,
                locked: true,
            },
        )
        .unwrap();
        // Existenzpruefung zuerst: unbekanntes Item meldet NotFound,
        // erst danach greift die Sperrpruefung fuer existierende Items.
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::RemoveSceneItem {
                    scene_id: scene,
                    item_id: Uuid::new_v4(),
                }
            ),
            Err(EngineError::NotFound("item"))
        ));
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::RemoveSceneItem {
                    scene_id: scene,
                    item_id
                }
            ),
            Err(EngineError::InvalidProject(message)) if message == "scene item locked"
        ));
        assert!(item(&project, scene, item_id).locked);
    }

    // -------------------------------------------------------------
    // Deterministische Eigenschaftstests (Round 8)
    //
    // Drei Invarianten der EngineCommand-Zustandsmaschine, geprueft
    // ueber 500 pseudozufaellige Befehlssequenzen (5 Seeds x 100
    // Sequenzen mit je 3-10 Befehlen). Ids stammen aus einem
    // zaehlerbasierten, deterministischen Raum; der handgerollte LCG
    // ist die einzige Zufallsquelle, damit jeder Fehler ueber Seed +
    // Sequenz- + Befehlsindex bitgenau reproduzierbar bleibt.
    // -------------------------------------------------------------

    use crate::project::{AudioSessionBinding, DisplayBinding, WindowBinding};

    /// Handgerollter LCG (Numerical-Recipes-Konstanten). Keine Uhr
    /// und kein eingebetteter Zufall im Generator: Der Seed bestimmt
    /// jede Generator-Entscheidung. Lediglich die Ids des
    /// Startprojekts stammen aus `Uuid::new_v4()` und variieren je
    /// Lauf - sie sind fuer die Assertionen irrelevant, denn lebende
    /// Ids zieht der Generator ausschliesslich aus dem Schattenabbild.
    struct Lcg(u64);

    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed)
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }

        /// Gleichverteilter Index in `0..n`; `n` muss positiv sein.
        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }

        /// Wahrscheinlichkeit in Prozent.
        fn chance(&mut self, percent: u64) -> bool {
            self.next_u64() % 100 < percent
        }
    }

    /// Deterministische Id aus einem festen 128-Bit-Raum. Frische und
    /// Geister-Ids liegen in disjunkten Bereichen und kollidieren
    /// weder untereinander noch mit den Zufall-Ids aus
    /// `ProjectV1::empty()`.
    fn deterministic_id(namespace: u128, n: u64) -> Uuid {
        Uuid::from_u128(namespace + n as u128)
    }
    const FRESH_NAMESPACE: u128 = 0xA11C_E5E0_0000_0000;
    const GHOST_NAMESPACE: u128 = 0xDEAD_5EED_0000_0000;

    /// Schattenabbild der Projekt-Id-Struktur, aus dem der Generator
    /// lebende Ids zieht. Wird ausschliesslich nach erfolgreichen
    /// apply()-Aufrufen fortgeschrieben und spiegelt damit exakt den
    /// realen Stand - kein Duplikat der apply()-Logik fuer Fehlpfade.
    struct ShadowState {
        scenes: Vec<Uuid>,
        sources: Vec<Uuid>,
        /// (scene_id, item_id, source_id)
        items: Vec<(Uuid, Uuid, Uuid)>,
        /// Gesperrte Items als Teilmenge von `items`; Zielmenge des
        /// Generators fuer die garantierte Verschraenkung von
        /// RemoveSource mit gesperrten Items.
        locked: Vec<(Uuid, Uuid, Uuid)>,
    }

    impl ShadowState {
        fn from_project(project: &ProjectV1) -> Self {
            Self {
                scenes: project.scenes.iter().map(|scene| scene.id).collect(),
                sources: project.sources.iter().map(|source| source.id()).collect(),
                items: project
                    .scenes
                    .iter()
                    .flat_map(|scene| {
                        scene
                            .items
                            .iter()
                            .map(move |item| (scene.id, item.id, item.source_id))
                    })
                    .collect(),
                locked: Vec::new(),
            }
        }

        fn live_scene(&self, rng: &mut Lcg) -> Uuid {
            self.scenes[rng.below(self.scenes.len())]
        }

        fn live_item(&self, rng: &mut Lcg) -> Option<(Uuid, Uuid)> {
            if self.items.is_empty() {
                return None;
            }
            let (scene_id, item_id, _) = self.items[rng.below(self.items.len())];
            Some((scene_id, item_id))
        }

        /// Aktualisiert den Schatten nach einem erfolgreichen Befehl.
        fn observe(&mut self, cmd: &EngineCommand) {
            match cmd {
                EngineCommand::AddScene { scene_id, .. } => self.scenes.push(*scene_id),
                EngineCommand::RemoveScene { scene_id } => {
                    self.scenes.retain(|s| *s != *scene_id);
                    self.items.retain(|(s, ..)| *s != *scene_id);
                    self.locked.retain(|(s, ..)| *s != *scene_id);
                }
                EngineCommand::AddSource { source } => self.sources.push(source.id()),
                EngineCommand::RemoveSource { source_id } => {
                    self.sources.retain(|s| *s != *source_id);
                    self.items.retain(|(.., src)| *src != *source_id);
                    self.locked.retain(|(.., src)| *src != *source_id);
                }
                // UpdateSource ersetzt unter unveraenderter Id: Schatten bleibt.
                EngineCommand::AddSceneItem {
                    scene_id,
                    item_id,
                    source_id,
                    ..
                } => self.items.push((*scene_id, *item_id, *source_id)),
                EngineCommand::RemoveSceneItem { scene_id, item_id } => {
                    let is_target =
                        |(s, i, _): &(Uuid, Uuid, Uuid)| *s == *scene_id && *i == *item_id;
                    self.items.retain(|triple| !is_target(triple));
                    self.locked.retain(|triple| !is_target(triple));
                }
                EngineCommand::SetItemLocked {
                    scene_id,
                    item_id,
                    locked,
                } => {
                    let found = self
                        .items
                        .iter()
                        .find(|(s, i, _)| *s == *scene_id && *i == *item_id)
                        .copied();
                    match (found, *locked) {
                        (Some(triple), true) => {
                            if !self.locked.contains(&triple) {
                                self.locked.push(triple);
                            }
                        }
                        (Some(_), false) => self.locked.retain(|triple| Some(*triple) != found),
                        _ => {}
                    }
                }
                // Laufzeit- und restliche Befehle aendern die Id-Struktur nicht.
                _ => {}
            }
        }
    }

    /// Befehlsgenerator: gewichtete Auswahl ueber die realen
    /// EngineCommand-Varianten. Lebende Ids kommen aus dem Schatten,
    /// dazwischen immer wieder Geister-Ids, die gezielt NotFound-
    /// Fehler (und damit die Atomicitaetspruefung) ausloesen.
    struct CommandGen {
        rng: Lcg,
        next_fresh: u64,
        next_ghost: u64,
    }

    impl CommandGen {
        fn new(seed: u64, sequence: usize) -> Self {
            Self {
                rng: Lcg::new(seed ^ (sequence as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
                next_fresh: 0,
                next_ghost: 0,
            }
        }

        fn fresh_id(&mut self) -> Uuid {
            self.next_fresh += 1;
            deterministic_id(FRESH_NAMESPACE, self.next_fresh)
        }

        fn ghost_id(&mut self) -> Uuid {
            self.next_ghost += 1;
            deterministic_id(GHOST_NAMESPACE, self.next_ghost)
        }

        /// 70% lebende Id (falls vorhanden), sonst Geister-Id.
        fn pick_id(&mut self, live: &[Uuid]) -> Uuid {
            if !live.is_empty() && self.rng.chance(70) {
                live[self.rng.below(live.len())]
            } else {
                self.ghost_id()
            }
        }

        /// Immer validate()-konforme Transformation: endliche Werte,
        /// Groesse in (0, 8192], Negativ-Crops ausgeschlossen und
        /// Crop-Summen klein gegenueber Breite/Hoehe.
        fn random_transform(&mut self) -> Transform {
            Transform {
                x: self.rng.below(400) as f32 - 200.0,
                y: self.rng.below(400) as f32 - 200.0,
                width: 100.0 + self.rng.below(1000) as f32,
                height: 100.0 + self.rng.below(1000) as f32,
                rotation_degrees: self.rng.below(360) as f32 - 180.0,
                crop_top: self.rng.below(20) as f32,
                crop_right: self.rng.below(20) as f32,
                crop_bottom: self.rng.below(20) as f32,
                crop_left: self.rng.below(20) as f32,
                opacity: [0.0, 0.25, 0.5, 0.75, 1.0][self.rng.below(5)],
            }
        }

        /// Gueltige Quelle in einer von sechs Varianten; alle Felder
        /// erfuellen validate() (Namen nichtleer, Volumen in [0,1],
        /// hex-Farben, Schriftgroesse im Bereich).
        fn source_variant(&mut self, variant: usize, id: Uuid) -> Source {
            match variant % 6 {
                0 => Source::Window {
                    id,
                    name: "Fenster".into(),
                    binding: WindowBinding {
                        process_path: "C:\\Spiele\\demo.exe".into(),
                        window_title: "Demofenster".into(),
                    },
                },
                1 => Source::Display {
                    id,
                    name: "Display".into(),
                    binding: DisplayBinding {
                        adapter_luid: "0x00010000".into(),
                        output_id: self.rng.below(2) as u32,
                    },
                },
                2 => Source::Image {
                    id,
                    name: "Bild".into(),
                    path: "/tmp/bild.png".into(),
                },
                3 => text_source(id),
                4 => media_source(id),
                _ => Source::ApplicationAudio {
                    id,
                    name: "Anwendung".into(),
                    binding: AudioSessionBinding {
                        process_path: "C:\\Apps\\player.exe".into(),
                        session_grouping_id: "sitzung".into(),
                    },
                    volume: 1.0,
                    muted: false,
                },
            }
        }

        /// RemoveSource mit Kaskaden-Fokus: Solange gesperrte Items
        /// getrackt werden, zielt die Auswahl zu 50% direkt auf eine
        /// Quelle, deren Kaskade mindestens eine dieser Sperren
        /// mitreisst; sonst wie ueblich - 70% lebende Id, sonst
        /// Geister-Id, damit NotFound weiterhin vorkommt
        /// (Atomicitaetsarm).
        fn remove_source(&mut self, state: &ShadowState) -> EngineCommand {
            let mut locked_owners: Vec<Uuid> = Vec::new();
            for (_, _, source_id) in &state.locked {
                if !locked_owners.contains(source_id) {
                    locked_owners.push(*source_id);
                }
            }
            let source_id = if !locked_owners.is_empty() && self.rng.chance(50) {
                locked_owners[self.rng.below(locked_owners.len())]
            } else {
                self.pick_id(&state.sources)
            };
            EngineCommand::RemoveSource { source_id }
        }

        fn next_command(&mut self, state: &ShadowState) -> EngineCommand {
            // Garantierte Verschraenkung, zweiter Teil: Existieren
            // gesperrte Items, bekommt RemoveSource erhoehte
            // Prioritaet, damit die Kaskade sie auch wirklich
            // erreicht - bevor die Sequenz endet oder ein Unlock das
            // Fenster schliesst.
            if !state.locked.is_empty() && self.rng.chance(40) {
                return self.remove_source(state);
            }
            match self.rng.below(100) {
                0..=9 => {
                    let variant = self.rng.below(6);
                    let id = self.fresh_id();
                    EngineCommand::AddSource {
                        source: self.source_variant(variant, id),
                    }
                }
                10..=15 => self.remove_source(state),
                16..=21 => {
                    let variant = self.rng.below(6);
                    let id = self.pick_id(&state.sources);
                    EngineCommand::UpdateSource {
                        source: self.source_variant(variant, id),
                    }
                }
                22..=31 => EngineCommand::AddScene {
                    scene_id: self.fresh_id(),
                    name: format!("Szene {}", self.next_fresh),
                },
                32..=37 => EngineCommand::RemoveScene {
                    scene_id: self.pick_id(&state.scenes),
                },
                38..=42 => EngineCommand::RenameScene {
                    scene_id: self.pick_id(&state.scenes),
                    name: format!("Szene {}", self.next_fresh),
                },
                43..=47 => EngineCommand::SetActiveScene {
                    scene_id: self.pick_id(&state.scenes),
                },
                48..=51 => EngineCommand::SetSceneHotkey {
                    scene_id: self.pick_id(&state.scenes),
                    // Eigene "Shift+F{n}"-Familie: kollidiert nie mit den
                    // Ctrl+Alt-Hotkeys aus ProjectV1::empty(). Der Zaehler
                    // wird pro Some-Hotkey verbraucht, damit zwei
                    // SetSceneHotkey-Befehle ohne dazwischenliegende
                    // fresh_id-Zuege nie denselben Wert erzeugen.
                    hotkey: if self.rng.chance(50) {
                        self.next_fresh += 1;
                        Some(format!("Shift+F{}", self.next_fresh))
                    } else {
                        None
                    },
                },
                52..=63 => {
                    // AddSceneItem: die Quelle MUSS existieren, sonst
                    // erzeugt apply() erfolgreich ein Waisen-Item und
                    // verletzt die Gueltigkeit. Geister-Ids sind hier
                    // bewusst verboten.
                    if state.sources.is_empty() {
                        let variant = self.rng.below(6);
                        let id = self.fresh_id();
                        return EngineCommand::AddSource {
                            source: self.source_variant(variant, id),
                        };
                    }
                    let scene_id = state.live_scene(&mut self.rng);
                    let item_id = self.fresh_id();
                    let source_id = state.sources[self.rng.below(state.sources.len())];
                    let transform = self.random_transform();
                    EngineCommand::AddSceneItem {
                        scene_id,
                        item_id,
                        source_id,
                        transform,
                    }
                }
                64..=79 => {
                    let live = state.live_item(&mut self.rng);
                    let (scene_id, item_id) = match live {
                        Some(pair) if self.rng.chance(70) => pair,
                        _ => {
                            let scene_id = state.live_scene(&mut self.rng);
                            let item_id = self.ghost_id();
                            (scene_id, item_id)
                        }
                    };
                    // Verschraenkungs-Ankurbelung: Solange gar keine
                    // Sperren getrackt werden, ein lebendes Item aber
                    // gezogen wurde, erzwingt der Generator das
                    // SetItemLocked-Unterarm, damit gesperrte Items
                    // ueberhaupt haeufig genug entstehen. Sobald
                    // Sperren existieren, entscheidet wie gehabt der
                    // Wuerfel (inkl. Aufheben).
                    let sub = match live {
                        Some(_) if state.locked.is_empty() => 2,
                        _ => self.rng.below(5),
                    };
                    match sub {
                        0 => EngineCommand::RemoveSceneItem { scene_id, item_id },
                        1 => EngineCommand::SetItemVisible {
                            scene_id,
                            item_id,
                            visible: self.rng.chance(50),
                        },
                        2 => EngineCommand::SetItemLocked {
                            scene_id,
                            item_id,
                            locked: self.rng.chance(60),
                        },
                        3 => EngineCommand::ReorderSceneItem {
                            scene_id,
                            item_id,
                            index: self.rng.below(8),
                        },
                        _ => EngineCommand::SetTransform {
                            scene_id,
                            item_id,
                            transform: self.random_transform(),
                        },
                    }
                }
                80..=81 => EngineCommand::SetOutputConfig {
                    output: if self.rng.chance(50) {
                        OutputConfig {
                            width: 1280,
                            height: 720,
                            fps: 30,
                            background: "#101418".into(),
                        }
                    } else {
                        OutputConfig {
                            width: 1920,
                            height: 1080,
                            fps: 60,
                            background: "#101418".into(),
                        }
                    },
                },
                82..=84 => EngineCommand::SetMediaPlaying {
                    source_id: self.pick_id(&state.sources),
                    playing: self.rng.chance(50),
                },
                85..=87 => EngineCommand::MediaSeek {
                    source_id: self.pick_id(&state.sources),
                    position_seconds: self.rng.below(600) as f64,
                },
                88..=90 => EngineCommand::SetAudioVolume {
                    source_id: self.pick_id(&state.sources),
                    // Endliche Werte, auch ausserhalb [0,1]: apply()
                    // klemmt sie in den Gueltigkeitsbereich; NaN wuerde
                    // clamp ueberleben und validate() brechen.
                    volume: [0.0, 0.25, 0.5, 0.75, 1.0, 1.5, -0.5][self.rng.below(7)],
                },
                _ => EngineCommand::SetAudioMuted {
                    source_id: self.pick_id(&state.sources),
                    muted: self.rng.chance(50),
                },
            }
        }
    }

    /// Sequenz-Korpus: 5 Seeds x 100 Sequenzen = 500 Laeufe mit je
    /// 3-10 Befehlen. Der Runner klont vor jedem apply(), wertet den
    /// Kaskaden-Zaehler vor dem Schatten-Fortschreiben aus, pflegt
    /// den Schatten nach jedem Erfolg fort und ruft den Callback mit
    /// (Seed, Sequenz, Befehlsindex, Befehl, Projekt danach, Snapshot
    /// davor, Schritt-Ergebnis) - so teilen sich alle drei
    /// Invariantentests exakt dieselbe deterministische Befehlsmenge.
    const SEEDS: [u64; 5] = [
        0x5EED_0001,
        0x5EED_0002,
        0x5EED_0003,
        0x5EED_0004,
        0x5EED_0005,
    ];
    const SEQUENCES_PER_SEED: usize = 100;

    /// Ergebnis eines einzelnen apply()-Aufrufs fuer die Invarianten.
    struct StepOutcome<'a> {
        ok: bool,
        /// Schattenstand nach diesem Schritt (bei Fehlschlaegen
        /// unveraendert).
        shadow: &'a ShadowState,
    }

    fn for_every_generated_command(
        mut on_command: impl FnMut(
            u64,
            usize,
            usize,
            &EngineCommand,
            &ProjectV1,
            &ProjectV1,
            &StepOutcome,
        ),
    ) -> usize {
        let mut locked_cascade_removals = 0usize;
        for &seed in &SEEDS {
            for sequence in 0..SEQUENCES_PER_SEED {
                let mut generator = CommandGen::new(seed, sequence);
                let mut project = ProjectV1::empty();
                let mut shadow = ShadowState::from_project(&project);
                let length = 3 + generator.rng.below(8);
                for command_index in 0..length {
                    let cmd = generator.next_command(&shadow);
                    let before = project.clone();
                    let ok = apply(&mut project, cmd.clone()).is_ok();
                    // Vor observe() auswerten: Danach haette der
                    // Schatten die kaskadierend entfernte Sperre
                    // bereits vergessen.
                    let mut removed_locked_item = false;
                    if ok
                        && let EngineCommand::RemoveSource { source_id } = &cmd
                        && shadow.locked.iter().any(|(.., src)| src == source_id)
                    {
                        removed_locked_item = true;
                    }
                    if ok {
                        shadow.observe(&cmd);
                    }
                    on_command(
                        seed,
                        sequence,
                        command_index,
                        &cmd,
                        &project,
                        &before,
                        &StepOutcome {
                            ok,
                            shadow: &shadow,
                        },
                    );
                    locked_cascade_removals += usize::from(removed_locked_item);
                }
            }
        }
        locked_cascade_removals
    }

    fn failure_context(
        seed: u64,
        sequence: usize,
        command_index: usize,
        cmd: &EngineCommand,
    ) -> String {
        format!("Seed {seed:#x}, Sequenz {sequence}, Befehl {command_index}: {cmd:?}")
    }

    /// Invariante 1 (Gueltigkeit): Nach jedem erfolgreichen apply()
    /// meldet ProjectV1::validate() keinen Befund.
    #[test]
    fn random_command_sequences_keep_project_valid() {
        for_every_generated_command(|seed, sequence, command_index, cmd, project, _, outcome| {
            if outcome.ok
                && let Err(err) = project.validate()
            {
                panic!(
                    "ungueltiges Projekt nach erfolgreichem apply(): {err} ({})",
                    failure_context(seed, sequence, command_index, cmd)
                );
            }
        });
    }

    /// Invariante 2 (Atomicitaet): Ein mit Err abgelehntes apply()
    /// hinterlaesst ein bytegleiches Projekt (Clone/PartialEq-
    /// Vergleich gegen den Snapshot von vor dem Aufruf).
    #[test]
    fn failed_apply_preserves_project_state_exactly() {
        for_every_generated_command(
            |seed, sequence, command_index, cmd, project, before, outcome| {
                if !outcome.ok && project != before {
                    panic!(
                        "fehlgeschlagenes apply() hat das Projekt veraendert:\n\
                     -- davor --\n{before:?}\n\
                     -- danach --\n{project:?}\n\
                     ({})",
                        failure_context(seed, sequence, command_index, cmd)
                    );
                }
            },
        );
    }

    /// Invariante 3 (Schattentreue mit garantierter Verschraenkung):
    /// Nach jedem erfolgreichen Befehl ist die (Szene, Item,
    /// Quelle)-Multimenge des Projekts exakt gleich dem Schatten.
    /// Waisen-Items, von der RemoveSource-Kaskade verlorene Items und
    /// Gleichheit - ein strengeres Praedikat als das bloesse
    /// Waisen-Pruefen von validate(). Der Generator lenkt RemoveSource
    /// gezielt auf Quellen mit gesperrten Items; der abschliessende
    /// Zaehler beweist, dass die Verschraenkung tatsaechlich
    /// stattfand statt nur moeglich zu sein.
    #[test]
    fn random_cascades_leave_no_orphaned_scene_items() {
        let locked_cascade_removals = for_every_generated_command(
            |seed, sequence, command_index, cmd, project, _, outcome| {
                if outcome.ok {
                    let mut actual: Vec<(Uuid, Uuid, Uuid)> = project
                        .scenes
                        .iter()
                        .flat_map(|scene| {
                            scene
                                .items
                                .iter()
                                .map(move |item| (scene.id, item.id, item.source_id))
                        })
                        .collect();
                    actual.sort_unstable();
                    let mut expected = outcome.shadow.items.clone();
                    expected.sort_unstable();
                    assert!(
                        actual == expected,
                        "Item-Multimenge weicht vom Schatten ab:\n\
                         -- Projekt --\n{actual:?}\n\
                         -- Schatten --\n{expected:?}\n\
                         ({})",
                        failure_context(seed, sequence, command_index, cmd)
                    );
                }
            },
        );
        assert!(
            locked_cascade_removals > 0,
            "Keine einzige RemoveSource-Kaskade hat ein gesperrtes \
             Item mitgerissen - die garantierte Verschraenkung ist \
             entfallen"
        );
    }
    #[test]
    fn set_audio_volume_clamps_finite_out_of_range_values() {
        let mut project = ProjectV1::empty();
        let media = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: media_source(media),
            },
        )
        .unwrap();
        for (requested, expected) in [(1.7_f32, 1.0), (-0.25, 0.0)] {
            apply(
                &mut project,
                EngineCommand::SetAudioVolume {
                    source_id: media,
                    volume: requested,
                },
            )
            .unwrap();
            match project.sources.iter().find(|source| source.id() == media) {
                Some(Source::Media { volume, .. }) => assert_eq!(*volume, expected),
                other => panic!("unerwartete Quelle nach Volume-Clamp: {other:?}"),
            }
        }
    }

    /// command() validiert nach apply(); NaN uebersteht das Clamp (Vergleiche
    /// mit NaN sind false) und muss an der Projektvalidierung mit der
    /// festgepinnten Meldung scheitern.
    #[test]
    fn set_audio_volume_nan_fails_validation_with_pinned_message() {
        let mut project = ProjectV1::empty();
        let media = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: media_source(media),
            },
        )
        .unwrap();
        apply(
            &mut project,
            EngineCommand::SetAudioVolume {
                source_id: media,
                volume: f32::NAN,
            },
        )
        .unwrap();
        assert_eq!(project.validate(), Err("invalid source volume".into()));
    }

    #[test]
    fn set_audio_muted_requires_audio_capable_source() {
        let mut project = ProjectV1::empty();
        let text = Uuid::new_v4();
        apply(
            &mut project,
            EngineCommand::AddSource {
                source: text_source(text),
            },
        )
        .unwrap();
        assert!(matches!(
            apply(
                &mut project,
                EngineCommand::SetAudioMuted {
                    source_id: text,
                    muted: true
                }
            ),
            Err(EngineError::InvalidProject(message))
                if message == "source has no audio"
        ));
    }
}
