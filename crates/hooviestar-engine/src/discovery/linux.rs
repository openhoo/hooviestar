use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use pipewire::{
    self as pw,
    node::{Node, NodeListener},
    types::ObjectType,
};

use crate::{discovery::SourceCandidate, project::AudioSessionBinding};

/// Ruhefenster: Nach so viel Zeit ohne neues Enumerationsereignis wird beendet.
const DISCOVERY_IDLE_TIMEOUT: Duration = Duration::from_millis(200);
/// Harte Obergrenze für die gesamte Enumeration unabhängig von Ereignissen.
const DISCOVERY_MAX_WAIT: Duration = Duration::from_millis(1_500);

struct BoundNode {
    _listener: NodeListener,
    _node: Node,
}

pub fn enumerate_audio_nodes() -> Result<Vec<SourceCandidate>, String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|error| error.to_string())?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).map_err(|error| error.to_string())?;
    let core = context
        .connect_rc(None)
        .map_err(|error| error.to_string())?;
    let registry = core.get_registry_rc().map_err(|error| error.to_string())?;
    let candidates = Rc::new(RefCell::new(Vec::new()));
    let bound_nodes = Rc::new(RefCell::new(Vec::<BoundNode>::new()));
    let registry_weak = registry.downgrade();
    let listener_candidates = candidates.clone();
    let listener_nodes = bound_nodes.clone();
    // Zeitpunkt des letzten Enumerationsereignisses für den Ruhe-Timer.
    let last_activity = Rc::new(Cell::new(None::<Instant>));
    let listener_activity = last_activity.clone();
    let _listener = registry
        .add_listener_local()
        .global(move |object| {
            listener_activity.set(Some(Instant::now()));
            if object.type_ != ObjectType::Node {
                return;
            }
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            let Ok(node): Result<Node, _> = registry.bind(object) else {
                return;
            };
            let candidates = listener_candidates.clone();
            let info_activity = listener_activity.clone();
            let listener = node
                .add_listener_local()
                .info(move |info| {
                    info_activity.set(Some(Instant::now()));
                    let Some(properties) = info.props() else {
                        return;
                    };
                    if let Some(candidate) = candidate_from_props(info.id(), properties) {
                        let mut candidates = candidates.borrow_mut();
                        candidates.retain(|existing| match existing {
                            SourceCandidate::ApplicationAudio { runtime_id, .. } => {
                                runtime_id != candidate_runtime_id(&candidate)
                            }
                            _ => true,
                        });
                        candidates.push(candidate);
                    }
                })
                .register();
            listener_nodes.borrow_mut().push(BoundNode {
                _listener: listener,
                _node: node,
            });
        })
        .register();
    let weak = mainloop.downgrade();
    let started = Instant::now();
    let timer_activity = last_activity.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        let Some(mainloop) = weak.upgrade() else {
            return;
        };
        // Beenden, sobald die Enumeration ruhig ist oder die Obergrenze läuft.
        let quiescent = match timer_activity.get() {
            Some(last) => last.elapsed() >= DISCOVERY_IDLE_TIMEOUT,
            None => false,
        };
        if started.elapsed() >= DISCOVERY_MAX_WAIT || quiescent {
            mainloop.quit();
        }
    });
    timer
        .update_timer(Some(DISCOVERY_IDLE_TIMEOUT), Some(DISCOVERY_IDLE_TIMEOUT))
        .into_result()
        .map_err(|error| error.to_string())?;
    mainloop.run();
    drop(timer);
    let mut candidates = candidates.borrow().clone();
    candidates.sort_by(|left, right| candidate_name(left).cmp(candidate_name(right)));
    candidates.dedup_by(|left, right| left == right);
    Ok(candidates)
}

fn candidate_from_props(
    node_id: u32,
    properties: &pw::spa::utils::dict::DictRef,
) -> Option<SourceCandidate> {
    let media_class = properties.get("media.class").unwrap_or_default();
    if media_class != "Stream/Output/Audio" && media_class != "Audio/Source" {
        return None;
    }
    if properties.get("application.name") == Some("Hooviestar")
        || properties
            .get("node.name")
            .is_some_and(|name| name.starts_with("hooviestar"))
    {
        return None;
    }
    let process_id = properties
        .get("application.process.id")
        .and_then(|value| value.parse::<u32>().ok());
    let process_path = process_id
        .and_then(|process_id| std::fs::read_link(format!("/proc/{process_id}/exe")).ok())
        .or_else(|| {
            properties
                .get("application.process.binary")
                .map(PathBuf::from)
        })?;
    let grouping = properties
        .get("node.group")
        .or_else(|| properties.get("node.name"))
        .or_else(|| properties.get("application.process.session-id"))
        .unwrap_or("pipewire")
        .to_string();
    let name = properties
        .get("application.name")
        .or_else(|| properties.get("node.description"))
        .or_else(|| properties.get("node.name"))
        .unwrap_or("Anwendungs-Audio")
        .to_string();
    let serial = properties.get("object.serial").unwrap_or("unknown");
    Some(SourceCandidate::ApplicationAudio {
        runtime_id: format!("pipewire:{serial}:{node_id}"),
        name,
        binding: AudioSessionBinding {
            process_path: process_path.to_string_lossy().into_owned(),
            session_grouping_id: grouping,
        },
    })
}

fn candidate_runtime_id(candidate: &SourceCandidate) -> &str {
    match candidate {
        SourceCandidate::Window { runtime_id, .. }
        | SourceCandidate::Display { runtime_id, .. }
        | SourceCandidate::ApplicationAudio { runtime_id, .. } => runtime_id,
    }
}

fn candidate_name(candidate: &SourceCandidate) -> &str {
    match candidate {
        SourceCandidate::Window { name, .. }
        | SourceCandidate::Display { name, .. }
        | SourceCandidate::ApplicationAudio { name, .. } => name,
    }
}
