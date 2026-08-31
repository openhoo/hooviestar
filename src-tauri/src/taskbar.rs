use std::sync::Mutex;

use hooviestar_engine::{EngineEvent, engine::DeviceRecoveryPhase};
use tauri::{
    AppHandle, Manager,
    window::{ProgressBarState, ProgressBarStatus},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(debug_assertions, allow(dead_code))]
enum Activity {
    #[default]
    Idle,
    Indeterminate,
    Progress(u8),
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Signals {
    update: Activity,
    device: Activity,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Presentation {
    #[default]
    Idle,
    Indeterminate,
    Progress(u8),
    Error,
}

impl Signals {
    fn presentation(self) -> Presentation {
        if self.device == Activity::Error || self.update == Activity::Error {
            Presentation::Error
        } else if self.device == Activity::Indeterminate {
            Presentation::Indeterminate
        } else {
            match self.update {
                Activity::Idle => Presentation::Idle,
                Activity::Indeterminate => Presentation::Indeterminate,
                Activity::Progress(progress) => Presentation::Progress(progress),
                Activity::Error => Presentation::Error,
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct TaskbarState {
    signals: Mutex<Signals>,
}

impl TaskbarState {
    fn update(&self, app: &AppHandle, mutate: impl FnOnce(&mut Signals)) {
        let next = {
            let mut signals = self.signals.lock().expect("taskbar state mutex poisoned");
            let previous = signals.presentation();
            mutate(&mut signals);
            let next = signals.presentation();
            (previous != next).then_some(next)
        };
        if let Some(next) = next {
            apply(app, next);
        }
    }
}

#[cfg(not(debug_assertions))]
pub(crate) fn update_checking(app: &AppHandle) {
    app.state::<TaskbarState>()
        .update(app, |signals| signals.update = Activity::Indeterminate);
}

#[cfg(not(debug_assertions))]
pub(crate) fn update_progress(app: &AppHandle, progress: Option<u8>) {
    app.state::<TaskbarState>().update(app, |signals| {
        signals.update = progress
            .map(Activity::Progress)
            .unwrap_or(Activity::Indeterminate);
    });
}

#[cfg(not(debug_assertions))]
pub(crate) fn update_installing(app: &AppHandle) {
    update_checking(app);
}

#[cfg(not(debug_assertions))]
pub(crate) fn update_finished(app: &AppHandle) {
    app.state::<TaskbarState>()
        .update(app, |signals| signals.update = Activity::Idle);
}

#[cfg(not(debug_assertions))]
pub(crate) fn update_failed(app: &AppHandle) {
    app.state::<TaskbarState>()
        .update(app, |signals| signals.update = Activity::Error);
}

pub(crate) fn record_engine_event(app: &AppHandle, event: &EngineEvent) {
    let EngineEvent::DeviceRecovery { phase, .. } = event else {
        return;
    };
    app.state::<TaskbarState>().update(app, |signals| {
        signals.device = match phase {
            DeviceRecoveryPhase::Started => Activity::Indeterminate,
            DeviceRecoveryPhase::Succeeded => Activity::Idle,
            DeviceRecoveryPhase::Failed => Activity::Error,
        };
    });
}

fn apply(app: &AppHandle, presentation: Presentation) {
    let Some(studio) = app.get_webview_window("studio") else {
        eprintln!("[hooviestar] Studio window unavailable for taskbar status");
        return;
    };
    let progress = match presentation {
        Presentation::Idle => ProgressBarState {
            status: Some(ProgressBarStatus::None),
            progress: None,
        },
        Presentation::Indeterminate => ProgressBarState {
            status: Some(ProgressBarStatus::Indeterminate),
            progress: None,
        },
        Presentation::Progress(progress) => ProgressBarState {
            status: Some(ProgressBarStatus::Normal),
            progress: Some(u64::from(progress)),
        },
        Presentation::Error => ProgressBarState {
            status: Some(ProgressBarStatus::Error),
            progress: Some(100),
        },
    };
    if let Err(error) = studio.set_progress_bar(progress) {
        eprintln!("[hooviestar] failed to set taskbar progress: {error}");
    }

    #[cfg(target_os = "windows")]
    {
        let overlay = (presentation == Presentation::Error).then(error_overlay_icon);
        if let Err(error) = studio.set_overlay_icon(overlay) {
            eprintln!("[hooviestar] failed to set taskbar overlay icon: {error}");
        }
    }
}

#[cfg(target_os = "windows")]
fn error_overlay_icon() -> tauri::image::Image<'static> {
    const SIZE: u32 = 32;
    let mut rgba = vec![0_u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let distance = (dx * dx + dy * dy).sqrt();
            let alpha = ((15.0 - distance) * 255.0).clamp(0.0, 255.0) as u8;
            if alpha == 0 {
                continue;
            }
            let index = ((y * SIZE + x) * 4) as usize;
            rgba[index..index + 4].copy_from_slice(&[214, 64, 69, alpha]);
            let exclamation = (13..=18).contains(&x) && (7..=20).contains(&y)
                || (13..=18).contains(&x) && (24..=27).contains(&y);
            if exclamation {
                rgba[index..index + 4].copy_from_slice(&[255, 255, 255, alpha]);
            }
        }
    }
    tauri::image::Image::new_owned(rgba, SIZE, SIZE)
}

#[cfg(test)]
mod tests {
    use super::{Activity, Presentation, Signals};

    #[test]
    fn device_failure_survives_unrelated_updater_success() {
        let mut signals = Signals {
            device: Activity::Error,
            update: Activity::Indeterminate,
        };
        signals.update = Activity::Idle;
        assert_eq!(signals.presentation(), Presentation::Error);
    }

    #[test]
    fn recovery_outranks_update_download_progress() {
        let signals = Signals {
            device: Activity::Indeterminate,
            update: Activity::Progress(63),
        };
        assert_eq!(signals.presentation(), Presentation::Indeterminate);
    }

    #[test]
    fn completed_recovery_reveals_active_update_progress() {
        let signals = Signals {
            device: Activity::Idle,
            update: Activity::Progress(63),
        };
        assert_eq!(signals.presentation(), Presentation::Progress(63));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn error_overlay_is_a_nonempty_rgba_icon() {
        let icon = super::error_overlay_icon();
        assert_eq!((icon.width(), icon.height()), (32, 32));
        assert_eq!(icon.rgba().len(), 32 * 32 * 4);
        let (pixels, remainder) = icon.rgba().as_chunks::<4>();
        assert!(remainder.is_empty());
        assert!(pixels.iter().any(|pixel| pixel[3] > 0));
    }
}
