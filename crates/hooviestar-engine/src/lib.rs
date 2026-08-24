pub mod audio;
pub mod discovery;
pub mod engine;
pub mod lifecycle;
pub mod persistence;
pub mod platform;
pub mod project;
pub mod video;

pub use discovery::{SourceCandidate, SourceEnumeration};
pub use engine::{
    EngineCommand, EngineError, EngineEvent, EngineHandle, NativeSurfaceKind, NativeSurfaces,
};
pub use project::{OutputConfig, ProjectV1};
