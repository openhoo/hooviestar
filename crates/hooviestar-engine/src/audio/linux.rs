use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipeWireBinding {
    pub application_binary: PathBuf,
    pub media_role: Option<String>,
    pub stable_node_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipeWireNode {
    pub id: u32,
    pub application_binary: PathBuf,
    pub media_role: Option<String>,
    pub stable_node_name: String,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum PipeWireAudioError {
    #[error("PipeWire registry unavailable")]
    RegistryUnavailable,
    #[error("per-application audio node unavailable")]
    CapabilityUnavailable,
    #[error("audio source is offline")]
    Offline,
    #[error("audio source binding is ambiguous")]
    Ambiguous,
}

pub fn bind_unique_node(
    binding: &PipeWireBinding,
    nodes: &[PipeWireNode],
) -> Result<u32, PipeWireAudioError> {
    let mut matches = nodes.iter().filter(|node| {
        node.application_binary == binding.application_binary
            && node.media_role == binding.media_role
            && node.stable_node_name == binding.stable_node_name
    });
    let node = matches.next().ok_or(PipeWireAudioError::Offline)?;
    if matches.next().is_some() {
        return Err(PipeWireAudioError::Ambiguous);
    }
    Ok(node.id)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipeWireStreamState {
    Disconnected,
    Connecting,
    Streaming,
    Rebinding,
    Stopped,
}

pub struct PipeWireStream {
    state: PipeWireStreamState,
    node_id: Option<u32>,
}

impl PipeWireStream {
    pub fn disconnected() -> Self {
        Self {
            state: PipeWireStreamState::Disconnected,
            node_id: None,
        }
    }

    pub fn connect(&mut self, node_id: u32) -> Result<(), PipeWireAudioError> {
        if self.state == PipeWireStreamState::Stopped {
            return Err(PipeWireAudioError::CapabilityUnavailable);
        }
        self.state = PipeWireStreamState::Connecting;
        self.node_id = Some(node_id);
        Ok(())
    }

    pub fn streaming(&mut self) -> Result<(), PipeWireAudioError> {
        if self.state != PipeWireStreamState::Connecting {
            return Err(PipeWireAudioError::RegistryUnavailable);
        }
        self.state = PipeWireStreamState::Streaming;
        Ok(())
    }

    pub fn registry_removed(&mut self, node_id: u32) {
        if self.node_id == Some(node_id) {
            self.node_id = None;
            self.state = PipeWireStreamState::Rebinding;
        }
    }

    pub fn stop(&mut self) {
        self.node_id = None;
        self.state = PipeWireStreamState::Stopped;
    }

    pub fn state(&self) -> PipeWireStreamState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_ambiguous_pipewire_nodes() {
        let binding = PipeWireBinding {
            application_binary: "/usr/bin/game".into(),
            media_role: Some("Game".into()),
            stable_node_name: "game-output".into(),
        };
        let node = PipeWireNode {
            id: 7,
            application_binary: binding.application_binary.clone(),
            media_role: binding.media_role.clone(),
            stable_node_name: binding.stable_node_name.clone(),
        };
        assert_eq!(
            bind_unique_node(&binding, &[node.clone(), PipeWireNode { id: 8, ..node }]),
            Err(PipeWireAudioError::Ambiguous)
        );
    }
}
