//! ufm-core: shared identifiers, errors, and the domain event bus.
//!
//! Every crate in the workspace builds on these types. Nothing here touches
//! the filesystem, the network, or the UI — this crate is pure domain truth.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Identifiers — typed wrappers so a LocationId can never be passed where an
// ItemId is expected. All serialize as plain UUID strings.
// ---------------------------------------------------------------------------

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(LocationId); // a mounted root: local dir, cloud account, archive, search…
id_type!(ItemId); // a single file/folder inside a location
id_type!(JobId); // one resumable operation (copy, move, sync…)
id_type!(ThemeId); // one installed theme package
id_type!(PluginId); // one loaded plugin
id_type!(McpClientId); // one paired AI file-clerk client
id_type!(CollectionId); // one virtual collection / saved search / smart folder

// ---------------------------------------------------------------------------
// Errors — one error enum for the whole workspace so Tauri commands and the
// MCP server can render them uniformly.
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum UfmError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("provider error ({provider}): {message}")]
    Provider { provider: String, message: String },
    #[error("job failed at step {step}: {message}")]
    JobFailed { step: String, message: String },
    #[error("theme error: {0}")]
    Theme(String),
    #[error("plugin error: {0}")]
    Plugin(String),
    #[error("mcp error: {0}")]
    Mcp(String),
    #[error("database error: {0}")]
    Database(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, UfmError>;

// ---------------------------------------------------------------------------
// Domain events — the single bus the shell subscribes to. UI state may be
// transient; everything durable is owned by the Rust core and announced here.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DomainEvent {
    JobProgress {
        job: JobId,
        completed_steps: u32,
        total_steps: u32,
    },
    JobFinished {
        job: JobId,
        outcome: JobOutcome,
    },
    CatalogChanged {
        location: LocationId,
    },
    ThemeChanged {
        theme: ThemeId,
    },
    PluginChanged {
        plugin: PluginId,
        loaded: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobOutcome {
    Completed,
    Cancelled,
    Failed { reason: String },
}

/// Unix-ish timestamp helper used by journals and audit logs.
pub fn now_utc() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}
