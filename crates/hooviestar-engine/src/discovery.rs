use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;

use crate::project::{AudioSessionBinding, DisplayBinding, WindowBinding};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceEnumeration {
    pub candidates: Vec<SourceCandidate>,
    pub portal_selection_required: bool,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SourceCandidate {
    Window {
        runtime_id: String,
        name: String,
        binding: WindowBinding,
    },
    Display {
        runtime_id: String,
        name: String,
        binding: DisplayBinding,
    },
    ApplicationAudio {
        runtime_id: String,
        name: String,
        binding: AudioSessionBinding,
    },
}
