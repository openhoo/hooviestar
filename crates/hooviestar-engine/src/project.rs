use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectV1 {
    pub version: u32,
    pub output: OutputConfig,
    pub sources: Vec<Source>,
    pub scenes: Vec<Scene>,
    pub active_scene_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub background: String,
}
impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            background: "#101418".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowBinding {
    pub process_path: String,
    pub window_title: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayBinding {
    pub adapter_luid: String,
    pub output_id: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSessionBinding {
    pub process_path: String,
    pub session_grouping_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Source {
    Window {
        id: Uuid,
        name: String,
        binding: WindowBinding,
    },
    Display {
        id: Uuid,
        name: String,
        binding: DisplayBinding,
    },
    Image {
        id: Uuid,
        name: String,
        path: String,
    },
    Text {
        id: Uuid,
        name: String,
        text: String,
        font_family: String,
        font_size_px: f32,
        font_weight: u16,
        color: String,
        background_color: String,
        align: TextAlign,
    },
    Media {
        id: Uuid,
        name: String,
        path: String,
        #[serde(rename = "loop")]
        looped: bool,
        continue_when_hidden: bool,
        restart_on_show: bool,
        volume: f32,
        muted: bool,
    },
    ApplicationAudio {
        id: Uuid,
        name: String,
        binding: AudioSessionBinding,
        volume: f32,
        muted: bool,
    },
}
impl Source {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Window { id, .. }
            | Self::Display { id, .. }
            | Self::Image { id, .. }
            | Self::Text { id, .. }
            | Self::Media { id, .. }
            | Self::ApplicationAudio { id, .. } => *id,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Self::Window { name, .. }
            | Self::Display { name, .. }
            | Self::Image { name, .. }
            | Self::Text { name, .. }
            | Self::Media { name, .. }
            | Self::ApplicationAudio { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    pub id: Uuid,
    pub name: String,
    pub hotkey: Option<String>,
    pub items: Vec<SceneItem>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneItem {
    pub id: Uuid,
    pub source_id: Uuid,
    pub visible: bool,
    pub locked: bool,
    pub transform: Transform,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub rotation_degrees: f32,
    pub crop_top: f32,
    pub crop_right: f32,
    pub crop_bottom: f32,
    pub crop_left: f32,
    pub opacity: f32,
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 720.0,
            rotation_degrees: 0.0,
            crop_top: 0.0,
            crop_right: 0.0,
            crop_bottom: 0.0,
            crop_left: 0.0,
            opacity: 1.0,
        }
    }
}

impl ProjectV1 {
    pub fn empty() -> Self {
        let game = Uuid::new_v4();
        let video = Uuid::new_v4();
        let both = Uuid::new_v4();
        Self {
            version: 1,
            output: OutputConfig::default(),
            sources: vec![],
            scenes: vec![
                Scene {
                    id: game,
                    name: "Spiel".into(),
                    hotkey: Some("Ctrl+Alt+1".into()),
                    items: vec![],
                },
                Scene {
                    id: video,
                    name: "Video".into(),
                    hotkey: Some("Ctrl+Alt+2".into()),
                    items: vec![],
                },
                Scene {
                    id: both,
                    name: "Beides".into(),
                    hotkey: Some("Ctrl+Alt+3".into()),
                    items: vec![],
                },
            ],
            active_scene_id: game,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported project version".into());
        }
        if !matches!(
            (self.output.width, self.output.height),
            (1280, 720) | (1920, 1080)
        ) || !matches!(self.output.fps, 30 | 60)
        {
            return Err("unsupported output preset".into());
        }
        if !is_color(&self.output.background) {
            return Err("invalid background color".into());
        }
        if self.scenes.is_empty() {
            return Err("project requires a scene".into());
        }
        let mut all_ids = HashSet::new();
        let mut source_ids = HashSet::new();
        for source in &self.sources {
            if !source_ids.insert(source.id()) || !all_ids.insert(source.id()) {
                return Err("duplicate source id".into());
            }
            if source.name().trim().is_empty() {
                return Err("source name is empty".into());
            }
            if let Source::Text {
                color,
                background_color,
                ..
            } = source
                && (!is_color(color) || !is_color(background_color))
            {
                return Err("invalid text color".into());
            }
            let font_size_px = match source {
                Source::Text { font_size_px, .. } => Some(*font_size_px),
                _ => None,
            };
            if font_size_px.is_some_and(|size| !size.is_finite() || size <= 0.0 || size > 4096.0) {
                return Err("invalid text font size".into());
            }
            let volume = match source {
                Source::Media { volume, .. } | Source::ApplicationAudio { volume, .. } => {
                    Some(*volume)
                }
                _ => None,
            };
            if volume.is_some_and(|volume| !volume.is_finite() || !(0.0..=1.0).contains(&volume)) {
                return Err("invalid source volume".into());
            }
        }
        let mut scene_ids = HashSet::new();
        let mut hotkeys = HashSet::new();
        let mut item_ids = HashSet::new();
        for scene in &self.scenes {
            let mut scene_source_ids = HashSet::new();
            if scene.name.trim().is_empty() {
                return Err("scene name is empty".into());
            }
            if !scene_ids.insert(scene.id) || !all_ids.insert(scene.id) {
                return Err("duplicate scene id".into());
            }
            if let Some(hotkey) = &scene.hotkey {
                if hotkey.trim().is_empty() {
                    return Err("invalid scene hotkey".into());
                }
                if !hotkeys.insert(hotkey.to_ascii_lowercase()) {
                    return Err("duplicate scene hotkey".into());
                }
            }
            if scene.items.len() > 128 {
                return Err("scene has too many items".into());
            }
            for item in &scene.items {
                if !item_ids.insert(item.id) || !all_ids.insert(item.id) {
                    return Err("duplicate item id".into());
                }
                if !source_ids.contains(&item.source_id) {
                    return Err("scene item source is missing".into());
                }
                if !scene_source_ids.insert(item.source_id) {
                    return Err("source appears more than once in scene".into());
                }
                let t = item.transform;
                if ![
                    t.x,
                    t.y,
                    t.width,
                    t.height,
                    t.rotation_degrees,
                    t.crop_top,
                    t.crop_right,
                    t.crop_bottom,
                    t.crop_left,
                    t.opacity,
                ]
                .iter()
                .all(|v| v.is_finite())
                    || t.width <= 0.0
                    || t.height <= 0.0
                    || t.width > 8192.0
                    || t.height > 8192.0
                    || t.crop_top < 0.0
                    || t.crop_right < 0.0
                    || t.crop_bottom < 0.0
                    || t.crop_left < 0.0
                    || t.crop_left + t.crop_right >= t.width
                    || t.crop_top + t.crop_bottom >= t.height
                    || !(0.0..=1.0).contains(&t.opacity)
                {
                    return Err("invalid transform".into());
                }
            }
        }
        if !scene_ids.contains(&self.active_scene_id) {
            return Err("active scene is missing".into());
        }
        Ok(())
    }
}

fn is_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_project_has_three_hotkey_scenes() {
        let project = ProjectV1::empty();
        assert_eq!(
            project
                .scenes
                .iter()
                .map(|scene| (scene.name.as_str(), scene.hotkey.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("Spiel", Some("Ctrl+Alt+1")),
                ("Video", Some("Ctrl+Alt+2")),
                ("Beides", Some("Ctrl+Alt+3")),
            ]
        );
        project.validate().unwrap();
    }

    #[test]
    fn duplicate_hotkey_is_rejected() {
        let mut project = ProjectV1::empty();
        project.scenes[1].hotkey = project.scenes[0].hotkey.clone();
        assert_eq!(project.validate(), Err("duplicate scene hotkey".into()));
    }

    #[test]
    fn qualified_output_dimensions_and_frame_rates_are_independent() {
        for (width, height) in [(1280, 720), (1920, 1080)] {
            for fps in [30, 60] {
                let mut project = ProjectV1::empty();
                project.output = OutputConfig {
                    width,
                    height,
                    fps,
                    background: "#aBc123".into(),
                };
                project.validate().unwrap();
            }
        }
        for output in [
            OutputConfig {
                width: 2560,
                height: 1440,
                fps: 60,
                background: "#101418".into(),
            },
            OutputConfig {
                width: 1280,
                height: 720,
                fps: 24,
                background: "#101418".into(),
            },
        ] {
            let mut project = ProjectV1::empty();
            project.output = output;
            assert_eq!(project.validate(), Err("unsupported output preset".into()));
        }
    }

    #[test]
    fn invalid_audio_volume_is_rejected() {
        let mut project = ProjectV1::empty();
        project.sources.push(Source::ApplicationAudio {
            id: Uuid::new_v4(),
            name: "Audio".into(),
            binding: AudioSessionBinding {
                process_path: "/usr/bin/game".into(),
                session_grouping_id: "game".into(),
            },
            volume: f32::NAN,
            muted: false,
        });
        assert_eq!(project.validate(), Err("invalid source volume".into()));
    }

    #[test]
    fn blank_source_name_is_rejected() {
        let mut project = ProjectV1::empty();
        project.sources.push(Source::ApplicationAudio {
            id: Uuid::new_v4(),
            name: "   ".into(),
            binding: AudioSessionBinding {
                process_path: "/usr/bin/game".into(),
                session_grouping_id: "game".into(),
            },
            volume: 1.0,
            muted: false,
        });
        assert_eq!(project.validate(), Err("source name is empty".into()));
        match project.sources.first_mut() {
            Some(Source::ApplicationAudio { name, .. }) => *name = "Game-Audio".into(),
            _ => unreachable!("test pushed an ApplicationAudio source"),
        }
        project.validate().unwrap();
    }

    #[test]
    fn blank_hotkey_is_rejected() {
        let mut project = ProjectV1::empty();
        project.scenes[0].hotkey = Some("   ".into());
        assert_eq!(project.validate(), Err("invalid scene hotkey".into()));
        project.scenes[0].hotkey = Some("Ctrl+Alt+9".into());
        project.validate().unwrap();
    }

    #[test]
    fn duplicate_source_in_one_scene_is_rejected() {
        let mut project = ProjectV1::empty();
        let source_id = Uuid::new_v4();
        project.sources.push(Source::Image {
            id: source_id,
            name: "Bild".into(),
            path: "/tmp/image.png".into(),
        });
        project.scenes[0].items.extend([
            SceneItem {
                id: Uuid::new_v4(),
                source_id,
                visible: true,
                locked: false,
                transform: Transform::default(),
            },
            SceneItem {
                id: Uuid::new_v4(),
                source_id,
                visible: true,
                locked: false,
                transform: Transform::default(),
            },
        ]);
        assert_eq!(
            project.validate(),
            Err("source appears more than once in scene".into())
        );
    }
}
