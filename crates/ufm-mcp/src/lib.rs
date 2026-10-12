//! ufm-mcp: the built-in MCP server — the "AI file clerk".
//!
//! Spec §§19–20: a flagship 2026 differentiator. AI assistants connect
//! directly: search/read, organization/tagging, dedupe/compaction analysis,
//! integrity checks, corrupted-file repair assistance, lost-media recovery.
//!
//! Trust model (non-negotiable):
//! - client pairing (explicit user approval, per client)
//! - scoped grants (per-tool scopes, item/location scopes, expiry)
//! - approval broker (destructive ops pause for human approval)
//! - prompt-injection boundaries (tool results are data, never instructions)
//! - append-only audit log (every call, grant, approval, outcome)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ufm_core::{now_utc, LocationId, McpClientId, Result, UfmError};

// ---------------------------------------------------------------------------
// Tools the clerk exposes. Destructive tools require an approval grant.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTool {
    Search,
    ReadItem,
    GetMetadata,
    Tag,
    Organize,       // move into virtual collections / apply rules
    DedupeAnalyze,  // report only — never deletes by itself
    IntegrityCheck,
    RepairAssist,   // proposes repairs; applying needs approval
    RecoverMedia,   // lost-media recovery assistance
}

impl McpTool {
    pub fn destructive(self) -> bool {
        matches!(
            self,
            McpTool::Organize | McpTool::RepairAssist | McpTool::RecoverMedia
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            McpTool::Search => "search",
            McpTool::ReadItem => "read_item",
            McpTool::GetMetadata => "get_metadata",
            McpTool::Tag => "tag",
            McpTool::Organize => "organize",
            McpTool::DedupeAnalyze => "dedupe_analyze",
            McpTool::IntegrityCheck => "integrity_check",
            McpTool::RepairAssist => "repair_assist",
            McpTool::RecoverMedia => "recover_media",
        }
    }
}

// ---------------------------------------------------------------------------
// Grants — what one paired client may do, where, until when.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub client: McpClientId,
    pub tools: Vec<McpTool>,
    /// None = all locations. Some = restricted to these roots.
    pub location_scope: Option<Vec<LocationId>>,
    pub expires: chrono::DateTime<chrono::Utc>,
    pub approvals_required: bool, // destructive tools pause for human approval
}

impl Grant {
    pub fn allows(&self, tool: McpTool, location: Option<&LocationId>) -> bool {
        if now_utc() > self.expires {
            return false;
        }
        if !self.tools.contains(&tool) {
            return false;
        }
        match (&self.location_scope, location) {
            (Some(scope), Some(loc)) => scope.contains(loc),
            (Some(_), None) => false,
            (None, _) => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Audit — append-only. Every tool call, grant change, approval, and outcome
// lands here. The audit chain is itself queryable (spec §26 core records).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub at: chrono::DateTime<chrono::Utc>,
    pub client: McpClientId,
    pub tool: McpTool,
    pub args_json: String,
    pub outcome: AuditOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AuditOutcome {
    Allowed,
    Denied { reason: String },
    ApprovedByHuman { approver: String },
    Failed { error: String },
}

pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn append(&mut self, client: McpClientId, tool: McpTool, args: &str, outcome: AuditOutcome) {
        let seq = self.entries.len() as u64 + 1;
        self.entries.push(AuditEntry {
            seq,
            at: now_utc(),
            client,
            tool,
            args_json: args.to_string(),
            outcome,
        });
        // TODO(spec §26): persist to the catalog DB (mcp_audit table), fsync.
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Server — pairing, grants, and the approval broker.
// ---------------------------------------------------------------------------

pub struct McpServer {
    grants: HashMap<McpClientId, Grant>,
    pub audit: AuditLog,
    /// Clients waiting on the user to approve pairing.
    pending_pairings: Vec<McpClientId>,
}

impl McpServer {
    pub fn new() -> Self {
        Self {
            grants: HashMap::new(),
            audit: AuditLog::new(),
            pending_pairings: Vec::new(),
        }
    }

    /// A client asks to pair. Nothing is granted until the human approves —
    /// pairing requests never auto-approve (spec §20).
    pub fn request_pairing(&mut self) -> McpClientId {
        let id = McpClientId::new();
        self.pending_pairings.push(id);
        id
    }

    pub fn approve_pairing(&mut self, client: &McpClientId, grant: Grant) -> Result<()> {
        if !self.pending_pairings.contains(client) {
            return Err(UfmError::Mcp("no pending pairing for that client".into()));
        }
        self.pending_pairings.retain(|c| c != client);
        self.grants.insert(*client, grant);
        Ok(())
    }

    pub fn revoke(&mut self, client: &McpClientId) -> bool {
        self.grants.remove(client).is_some()
    }

    /// The single choke point for every tool call. Prompt-injection boundary:
    /// tool *results* are returned as data; the model never receives new
    /// instructions from file content through this path.
    pub fn authorize(
        &mut self,
        client: &McpClientId,
        tool: McpTool,
        location: Option<&LocationId>,
        args_json: &str,
    ) -> Authorization {
        let grant = match self.grants.get(client) {
            Some(g) => g,
            None => {
                self.audit.append(
                    *client,
                    tool,
                    args_json,
                    AuditOutcome::Denied {
                        reason: "client not paired".into(),
                    },
                );
                return Authorization::Denied("client not paired".into());
            }
        };
        if !grant.allows(tool, location) {
            self.audit.append(
                *client,
                tool,
                args_json,
                AuditOutcome::Denied {
                    reason: "outside grant scope".into(),
                },
            );
            return Authorization::Denied("outside grant scope".into());
        }
        if tool.destructive() && grant.approvals_required {
            // The call parks here until the human approves in the UI.
            return Authorization::NeedsHumanApproval;
        }
        self.audit
            .append(*client, tool, args_json, AuditOutcome::Allowed);
        Authorization::Allowed
    }
}

#[derive(Debug, Clone)]
pub enum Authorization {
    Allowed,
    NeedsHumanApproval,
    Denied(String),
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}
