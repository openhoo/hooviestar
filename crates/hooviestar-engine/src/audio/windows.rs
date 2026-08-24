use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionBinding {
    pub canonical_process_path: PathBuf,
    pub grouping_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessLoopbackRequest {
    pub process_id: u32,
    pub include_process_tree: bool,
    pub virtual_audio_device: &'static str,
    pub sample_rate: u32,
    pub channels: u16,
}

impl ProcessLoopbackRequest {
    pub fn for_process_tree(process_id: u32) -> Self {
        Self {
            process_id,
            include_process_tree: true,
            virtual_audio_device: "VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK",
            sample_rate: super::SAMPLE_RATE,
            channels: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureState {
    Idle,
    Activating,
    Capturing,
    Rebinding,
    Stopped,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum WindowsAudioError {
    #[error("audio session is offline")]
    Offline,
    #[error("audio session binding is ambiguous")]
    Ambiguous,
    #[error("invalid audio state transition from {0:?}")]
    InvalidTransition(CaptureState),
    #[error("audio device invalidated: HRESULT {0:#x}")]
    DeviceInvalidated(i32),
}

pub struct ProcessLoopbackSession {
    binding: SessionBinding,
    state: CaptureState,
    active_session_instance: Option<String>,
}

impl ProcessLoopbackSession {
    pub fn new(binding: SessionBinding) -> Self {
        Self {
            binding,
            state: CaptureState::Idle,
            active_session_instance: None,
        }
    }

    pub fn bind_unique(
        &mut self,
        candidates: &[(SessionBinding, String)],
    ) -> Result<String, WindowsAudioError> {
        let mut matches = candidates
            .iter()
            .filter(|(candidate, _)| candidate == &self.binding);
        let (_, instance_id) = matches.next().ok_or(WindowsAudioError::Offline)?;
        if matches.next().is_some() {
            return Err(WindowsAudioError::Ambiguous);
        }
        self.active_session_instance = Some(instance_id.clone());
        Ok(instance_id.clone())
    }

    pub fn begin_activation(
        &mut self,
        process_id: u32,
    ) -> Result<ProcessLoopbackRequest, WindowsAudioError> {
        if self.state != CaptureState::Idle && self.state != CaptureState::Rebinding {
            return Err(WindowsAudioError::InvalidTransition(self.state));
        }
        self.state = CaptureState::Activating;
        Ok(ProcessLoopbackRequest::for_process_tree(process_id))
    }

    pub fn activated(&mut self) -> Result<(), WindowsAudioError> {
        if self.state != CaptureState::Activating {
            return Err(WindowsAudioError::InvalidTransition(self.state));
        }
        self.state = CaptureState::Capturing;
        Ok(())
    }

    pub fn device_invalidated(&mut self, hresult: i32) -> WindowsAudioError {
        self.state = CaptureState::Rebinding;
        WindowsAudioError::DeviceInvalidated(hresult)
    }

    pub fn stop(&mut self) {
        self.state = CaptureState::Stopped;
        self.active_session_instance = None;
    }

    pub fn state(&self) -> CaptureState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> SessionBinding {
        SessionBinding {
            canonical_process_path: "C:\\Games\\game.exe".into(),
            grouping_id: "game".into(),
        }
    }

    #[test]
    fn never_binds_ambiguous_sessions() {
        let mut session = ProcessLoopbackSession::new(binding());
        let candidates = vec![(binding(), "one".into()), (binding(), "two".into())];
        assert_eq!(
            session.bind_unique(&candidates),
            Err(WindowsAudioError::Ambiguous)
        );
    }

    #[test]
    fn invalidation_requires_reactivation() {
        let mut session = ProcessLoopbackSession::new(binding());
        let request = session.begin_activation(42).unwrap();
        assert!(request.include_process_tree);
        session.activated().unwrap();
        session.device_invalidated(0x88890004u32 as i32);
        assert_eq!(session.state(), CaptureState::Rebinding);
        session.begin_activation(43).unwrap();
    }
}
