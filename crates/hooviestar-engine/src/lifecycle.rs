#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryStep {
    StopCaptureAndMedia,
    ReleaseGpuResources,
    CreateGraphicsDevice,
    CreateMediaDeviceManager,
    CreateFramePools,
    CreateDecoders,
    CreateSwapchains,
    RebindScene,
}

pub const DEVICE_RECOVERY_ORDER: [RecoveryStep; 8] = [
    RecoveryStep::StopCaptureAndMedia,
    RecoveryStep::ReleaseGpuResources,
    RecoveryStep::CreateGraphicsDevice,
    RecoveryStep::CreateMediaDeviceManager,
    RecoveryStep::CreateFramePools,
    RecoveryStep::CreateDecoders,
    RecoveryStep::CreateSwapchains,
    RecoveryStep::RebindScene,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownStep {
    BlockHotkeys,
    FlushProject,
    StopAudioAndRestoreSessions,
    StopMediaAndCapture,
    StopRenderAndSwapchains,
    ReleaseGraphicsAndMediaFoundation,
    AcknowledgeWatchdog,
    CloseApplication,
}

pub const SHUTDOWN_ORDER: [ShutdownStep; 8] = [
    ShutdownStep::BlockHotkeys,
    ShutdownStep::FlushProject,
    ShutdownStep::StopAudioAndRestoreSessions,
    ShutdownStep::StopMediaAndCapture,
    ShutdownStep::StopRenderAndSwapchains,
    ShutdownStep::ReleaseGraphicsAndMediaFoundation,
    ShutdownStep::AcknowledgeWatchdog,
    ShutdownStep::CloseApplication,
];

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum LifecycleError {
    #[error("lifecycle step out of order: expected {expected}, received {received}")]
    OutOfOrder { expected: usize, received: usize },
    #[error("lifecycle already completed")]
    Completed,
}

pub struct OrderedLifecycle<T: Copy + Eq + 'static> {
    order: &'static [T],
    next: usize,
}

impl<T: Copy + Eq + 'static> OrderedLifecycle<T> {
    pub const fn new(order: &'static [T]) -> Self {
        Self { order, next: 0 }
    }

    pub fn advance(&mut self, step: T) -> Result<(), LifecycleError> {
        let Some(expected) = self.order.get(self.next) else {
            return Err(LifecycleError::Completed);
        };
        if *expected != step {
            let received = self
                .order
                .iter()
                .position(|candidate| *candidate == step)
                .unwrap_or(usize::MAX);
            return Err(LifecycleError::OutOfOrder {
                expected: self.next,
                received,
            });
        }
        self.next += 1;
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        self.next == self.order.len()
    }
}

pub fn device_recovery() -> OrderedLifecycle<RecoveryStep> {
    OrderedLifecycle::new(&DEVICE_RECOVERY_ORDER)
}

pub fn shutdown() -> OrderedLifecycle<ShutdownStep> {
    OrderedLifecycle::new(&SHUTDOWN_ORDER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_recovery_cannot_reorder_resources() {
        let mut lifecycle = device_recovery();
        assert_eq!(
            lifecycle.advance(RecoveryStep::CreateGraphicsDevice),
            Err(LifecycleError::OutOfOrder {
                expected: 0,
                received: 2,
            })
        );
        for step in DEVICE_RECOVERY_ORDER {
            lifecycle.advance(step).unwrap();
        }
        assert!(lifecycle.is_complete());
    }

    #[test]
    fn shutdown_restores_audio_before_render_exit() {
        let restore = SHUTDOWN_ORDER
            .iter()
            .position(|step| *step == ShutdownStep::StopAudioAndRestoreSessions)
            .unwrap();
        let render = SHUTDOWN_ORDER
            .iter()
            .position(|step| *step == ShutdownStep::StopRenderAndSwapchains)
            .unwrap();
        assert!(restore < render);
    }
}
