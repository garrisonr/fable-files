//! mcp_server: the MCP "AI file clerk" over stdio (Phase 3).
//!
//! Contract: docs/MCP_API.md. Transport is newline-delimited JSON-RPC
//! 2.0 on stdin/stdout (MCP protocol 2024-11-05); stdout carries ONLY
//! protocol frames — every diagnostic goes to stderr.
//!
//! Trust model, as the doc demands: `ufm_mcp::McpServer::authorize`
//! is the single choke point for every tool call. Pairing for a stdio
//! client is the launch itself (the process exists only because the
//! owner configured it) plus the `mcp-grants.json` grant file in the
//! data dir, which the owner can narrow or revoke at any time.
//! Destructive tools park at the approval broker: the grant requires
//! human approval and stdio has no approval channel in v1, so those
//! calls are refused with a plain-language reason — and audited.
//!
//! Every call (allowed / denied / parked / failed) is persisted to
//! the catalog's `mcp_audit` table via `Catalog::append_mcp_audit`;
//! read it back with `fable-files --mcp-audit` or the
//! `mcp_audit_recent` Tauri command. Retention: newest 10,000 rows.
//!
//! Handlers run against the same catalog/search machinery the GUI
//! uses — `ufm_search` parsing + facets, `Catalog::search_fts`, the
//! path-keyed label/star stores, Library Memory volume badges. No
//! parallel data, no fake results.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};
use ufm_catalog::Catalog;
use ufm_core::{CollectionId, ItemId};
use ufm_mcp::{Authorization, McpServer, McpTool};

use crate::commands;

const PROTOCOL_VERSION: &str = "2024-11-05";
/// Audit retention: keep the newest this many `mcp_audit` rows.
const AUDIT_KEEP: u64 = 10_000;
/// Cap stored argument JSON so a hostile/huge call can't bloat the
/// audit chain. Truncation is marked in the stored string.
const ARGS_JSON_CAP: usize = 2_000;
/// `read_item` per-call byte cap.
const READ_CAP: u64 = 64 * 1024;

// ---------------------------------------------------------------------------
// Paths + entry points
// ---------------------------------------------------------------------------

/// The app-data base the GUI uses: Tauri `app_data_dir()` (=
/// `$XDG_DATA_HOME/com.garrisonr.fablefiles` or
/// `~/.local/share/com.garrisonr.fablefiles`) + "fable-files".
/// Replicated here because the headless modes run before Tauri.
pub fn data_base() -> PathBuf {
    let root = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".local").join("share")
        });
    root.join("com.garrisonr.fablefiles").join("fable-files")
}

fn open_catalog() -> Result<(PathBuf, Catalog), String> {
    let base = data_base();
    let path = base.join("catalog.db");
    if !path.exists() {
        return Err(format!(
            "no catalog at {} — run Fable Files once so it can index something first",
            path.display()
        ));
    }
    let catalog = Catalog::open(&path).map_err(|e| format!("open catalog: {e}"))?;
    Ok((base, catalog))
}

/// `fable-files mcp` — serve MCP on stdio until EOF. Exit codes:
/// 0 clean EOF, 2 catalog could not be opened.
pub fn run_mcp_stdio() -> i32 {
    let (base, catalog) = match open_catalog() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fable-files mcp: {e}");
            return 2;
        }
    };
    let mut clerk = match Clerk::new(base, catalog) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fable-files mcp: {e}");
            return 2;
        }
    };
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout().lock();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return 0, // EOF: the client hung up.
            Ok(_) => {
                if let Some(response) = clerk.handle_line(&line) {
                    let _ = writeln!(stdout, "{response}");
                    let _ = stdout.flush();
                }
            }
            Err(e) => {
                eprintln!("fable-files mcp: stdin read failed: {e}");
                return 2;
            }
        }
    }
}

/// `fable-files --mcp-audit [n]` — print the newest n audit rows
/// (default 20) as plain text. The CLI read path for the audit chain.
pub fn run_mcp_audit_cli(limit: u32) -> i32 {
    let (_base, catalog) = match open_catalog() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fable-files --mcp-audit: {e}");
            return 2;
        }
    };
    match catalog.recent_mcp_audit(limit) {
        Ok(rows) => {
            if rows.is_empty() {
                println!("mcp_audit: no tool calls recorded yet");
                return 0;
            }
            for r in &rows {
                let args: String = r.args_json.chars().take(120).collect();
                println!(
                    "#{seq} {at} client={client} tool={tool} write={w} {ms}ms outcome={outcome}\n    args: {args}",
                    seq = r.seq,
                    at = r.at,
                    client = r.client,
                    tool = r.tool,
                    w = if r.is_write { "yes" } else { "no" },
                    ms = r.duration_ms,
                    outcome = r.outcome,
                );
            }
            0
        }
        Err(e) => {
            eprintln!("fable-files --mcp-audit: {e}");
            2
        }
    }
}

// ---------------------------------------------------------------------------
// Grants — the pairing record for the local stdio client
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct GrantsFile {
    /// None (or absent) = every tool. A list narrows this client to
    /// exactly those tool names (unknown names are ignored, loudly).
    #[serde(default)]
    tools: Option<Vec<String>>,
    /// Destructive tools park for human approval while true (and
    /// stdio cannot approve in v1, so they stay parked).
    #[serde(default = "default_true")]
    approvals_required: bool,
}

fn default_true() -> bool {
    true
}

const DEFAULT_GRANTS: &str = r#"{
  "comment": "Fable Files MCP grant for the local stdio client. The stdio server only exists when you (or your MCP client config) launch `fable-files mcp` — that launch IS the pairing; this file is the grant. Set \"tools\" to a list of tool names to narrow what this client may call. Destructive tools (organize, repair_assist, recover_media) park for human approval while \"approvals_required\" is true, and stdio has no approval channel in v1, so they stay parked either way. Delete this file to reset it to defaults.",
  "tools": null,
  "approvals_required": true
}
"#;

fn load_grants(base: &Path) -> GrantsFile {
    let path = base.join("mcp-grants.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!(
                "fable-files mcp: {} is not valid ({e}) — using defaults",
                path.display()
            );
            GrantsFile {
                tools: None,
                approvals_required: true,
            }
        }),
        Err(_) => {
            if let Err(e) = std::fs::write(&path, DEFAULT_GRANTS) {
                eprintln!("fable-files mcp: could not write {}: {e}", path.display());
            }
            GrantsFile {
                tools: None,
                approvals_required: true,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The clerk
// ---------------------------------------------------------------------------

struct Clerk {
    catalog: Catalog,
    server: McpServer,
    client: ufm_core::McpClientId,
    base: PathBuf,
    calls: u64,
}

impl Clerk {
    fn new(base: PathBuf, catalog: Catalog) -> Result<Self, String> {
        let grants = load_grants(&base);
        let tools: Vec<McpTool> = match &grants.tools {
            None => McpTool::ALL.to_vec(),
            Some(names) => names
                .iter()
                .filter_map(|n| match McpTool::from_name(n) {
                    Some(t) => Some(t),
                    None => {
                        eprintln!("fable-files mcp: ignoring unknown tool in grants: {n}");
                        None
                    }
                })
                .collect(),
        };
        let mut server = McpServer::new();
        let client = server.request_pairing();
        server
            .approve_pairing(
                &client,
                ufm_mcp::Grant {
                    client,
                    tools,
                    location_scope: None,
                    // The owner-local grant does not expire; the file
                    // (and the launch itself) is the control.
                    expires: ufm_core::now_utc() + chrono::Duration::days(36_500),
                    approvals_required: grants.approvals_required,
                },
            )
            .map_err(|e| format!("pairing the local client failed: {e}"))?;
        Ok(Self {
            catalog,
            server,
            client,
            base,
            calls: 0,
        })
    }

    /// One input line → one output line (or None for notifications).
    fn handle_line(&mut self, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                return Some(error_frame(
                    Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ))
            }
        };
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(|m| m.as_str()) else {
            // A response frame or a malformed request — only requests
            // with an id earn an error reply.
            return id.map(|id| error_frame(id, -32600, "invalid request: no method"));
        };
        if id.is_none() {
            // Notifications (initialized, cancelled, …) never reply.
            return None;
        }
        let id = id.unwrap();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let value = match method {
            "initialize" => json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "fable-files",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tool_definitions() }),
            "tools/call" => match self.call_tool(&params) {
                Ok(result) => result,
                Err((code, message)) => {
                    return Some(error_frame(id, code, &message));
                }
            },
            other => {
                return Some(error_frame(
                    id,
                    -32601,
                    &format!("method not found: {other}"),
                ));
            }
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": value }).to_string())
    }

    /// Execute one tools/call. Protocol-shape problems are JSON-RPC
    /// errors (Err tuple); everything the tool itself decides —
    /// denials included — is a CallToolResult with isError set.
    fn call_tool(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let Some(name) = params.get("name").and_then(|n| n.as_str()) else {
            return Err((-32602, "invalid params: tools/call needs a tool name".into()));
        };
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        if !args.is_object() {
            return Err((-32602, "invalid params: arguments must be an object".into()));
        }
        let Some(tool) = McpTool::from_name(name) else {
            return Ok(tool_error(&format!("unknown tool: {name}")));
        };
        let args_json = truncate_args(&args.to_string());
        // Location pre-resolution so a scoped grant can bite on the
        // item tools (the local grant is unscoped, but the choke
        // point stays honest if the owner narrows it later).
        let location = self.presolve_location(tool, &args);
        let started = Instant::now();
        let auth = self
            .server
            .authorize(&self.client, tool, location.as_ref(), &args_json);
        let result = match auth {
            Authorization::Denied(reason) => {
                self.audit(tool, &args_json, "denied", Some(&reason), started);
                tool_error(&format!("denied: {reason}"))
            }
            Authorization::NeedsHumanApproval => {
                let why = "this tool changes files and parks for human approval; \
                           the stdio server has no approval channel in v1 — \
                           run the action in the Fable Files app instead";
                self.audit(tool, &args_json, "needs_approval", Some(why), started);
                tool_error(why)
            }
            Authorization::Allowed => match self.dispatch(tool, &args) {
                Ok(value) => {
                    self.audit(tool, &args_json, "allowed", None, started);
                    json!({
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string_pretty(&value)
                                .unwrap_or_else(|_| value.to_string()),
                        }],
                        "isError": false,
                    })
                }
                Err(message) => {
                    self.audit(tool, &args_json, "failed", Some(&message), started);
                    tool_error(&message)
                }
            },
        };
        Ok(result)
    }

    fn presolve_location(
        &self,
        tool: McpTool,
        args: &Value,
    ) -> Option<ufm_core::LocationId> {
        if !matches!(tool, McpTool::ReadItem | McpTool::GetMetadata) {
            return None;
        }
        if let Some(raw) = str_arg(args, "item_id") {
            if let Ok(uuid) = uuid::Uuid::parse_str(raw) {
                if let Ok(Some(item)) = self.catalog.item_by_id(&ItemId(uuid)) {
                    return Some(item.location);
                }
            }
        }
        if let Some(path) = str_arg(args, "path") {
            if let Ok(Some(item)) = self.catalog.item_by_path(path) {
                return Some(item.location);
            }
        }
        None
    }

    fn audit(
        &mut self,
        tool: McpTool,
        args_json: &str,
        tag: &str,
        detail: Option<&str>,
        started: Instant,
    ) {
        let outcome = json!({ "outcome": tag, "detail": detail }).to_string();
        let ms = started.elapsed().as_millis() as i64;
        if let Err(e) = self.catalog.append_mcp_audit(
            &self.client.to_string(),
            tool.name(),
            args_json,
            &outcome,
            tool.is_write(),
            ms,
        ) {
            eprintln!("fable-files mcp: audit append failed: {e}");
        }
        self.calls += 1;
        if self.calls % 100 == 0 {
            let _ = self.catalog.prune_mcp_audit(AUDIT_KEEP);
        }
    }

    fn dispatch(&self, tool: McpTool, args: &Value) -> Result<Value, String> {
        match tool {
            McpTool::Search => tool_search(&self.catalog, args),
            McpTool::ReadItem => tool_read_item(&self.catalog, args),
            McpTool::GetMetadata => tool_get_metadata(&self.catalog, args),
            McpTool::Tag => tool_tag(&self.catalog, args),
            McpTool::DedupeAnalyze => tool_dedupe(&self.catalog, args),
            McpTool::IntegrityCheck => tool_integrity(&self.catalog),
            McpTool::ListLocations => tool_list_locations(&self.base),
            McpTool::ListVolumes => tool_list_volumes(&self.catalog),
            McpTool::ListCollections => tool_list_collections(&self.catalog),
            McpTool::CollectionItems => tool_collection_items(&self.catalog, args),
            McpTool::ListJobs => tool_list_jobs(&self.catalog, args),
            // Reachable only when the owner's grant waives approvals;
            // v1 still ships no filesystem-mutating MCP path.
            McpTool::Organize | McpTool::RepairAssist | McpTool::RecoverMedia => Err(
                format!(
                    "{} is not available over stdio in v1: it changes files, and file \
                     changes belong in the Fable Files app, where they run through \
                     the journaled job engine with preview + undo",
                    tool.name()
                ),
            ),
        }
    }
}

fn error_frame(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn truncate_args(s: &str) -> String {
    if s.len() <= ARGS_JSON_CAP {
        return s.to_string();
    }
    let mut cut = ARGS_JSON_CAP;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…[truncated]", &s[..cut])
}

// ---------------------------------------------------------------------------
// Tool definitions (tools/list)
// ---------------------------------------------------------------------------

fn tool_definitions() -> Vec<Value> {
    let obj = |props: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": props,
            "required": required,
        })
    };
    vec![
        json!({
            "name": "search",
            "description": "Search the Fable Files catalog with the one search language (name terms plus label: and starred: facets). Results carry labels, stars, and Library Memory volume badges — files on disconnected drives stay findable and are flagged offline.",
            "inputSchema": obj(json!({
                "query": { "type": "string", "description": "The search query, e.g. \"zelda label:roms\" or \"starred:true\"." },
                "limit": { "type": "integer", "description": "Max hits (default 50, max 500)." },
                "type_filter": { "type": "string", "description": "Optional: image | video | audio | document | archive | email." },
                "path_prefix": { "type": "string", "description": "Optional: only hits under this absolute path." },
            }), &["query"]),
        }),
        json!({
            "name": "read_item",
            "description": "Read a byte range of one catalogued file by item_id (from search). Capped at 64 KiB per call; returns UTF-8 text when the bytes are valid UTF-8, base64 otherwise.",
            "inputSchema": obj(json!({
                "item_id": { "type": "string" },
                "offset": { "type": "integer", "description": "Byte offset (default 0)." },
                "length": { "type": "integer", "description": "Bytes to read (default/max 65536)." },
            }), &["item_id"]),
        }),
        json!({
            "name": "get_metadata",
            "description": "Full catalog metadata for one item (by item_id or absolute path): kind, size, modified, labels, starred, media identity, and its volume badge.",
            "inputSchema": obj(json!({
                "item_id": { "type": "string" },
                "path": { "type": "string" },
            }), &[]),
        }),
        json!({
            "name": "tag",
            "description": "Add or remove a label on a file by absolute path — the same path-keyed label store the Fable Files UI writes. This is a write; every call is audited.",
            "inputSchema": obj(json!({
                "path": { "type": "string" },
                "label": { "type": "string" },
                "action": { "type": "string", "description": "\"add\" (default) or \"remove\"." },
            }), &["path", "label"]),
        }),
        json!({
            "name": "organize",
            "description": "DESTRUCTIVE (contract): move items into collections / apply rules. Parks for human approval; not available over stdio in v1.",
            "inputSchema": obj(json!({}), &[]),
        }),
        json!({
            "name": "dedupe_analyze",
            "description": "Report-only duplicate CANDIDATES: groups of files sharing an identical byte size. Content is not hashed — equal size does not prove equal content. Never deletes anything.",
            "inputSchema": obj(json!({
                "limit": { "type": "integer", "description": "Max groups (default 20, max 100)." },
            }), &[]),
        }),
        json!({
            "name": "integrity_check",
            "description": "Catalog health: SQLite integrity_check verdict plus record counts and database size.",
            "inputSchema": obj(json!({}), &[]),
        }),
        json!({
            "name": "repair_assist",
            "description": "DESTRUCTIVE (contract): propose/apply repairs. Parks for human approval; not available over stdio in v1.",
            "inputSchema": obj(json!({}), &[]),
        }),
        json!({
            "name": "recover_media",
            "description": "DESTRUCTIVE (contract): lost-media recovery assistance. Parks for human approval; not available over stdio in v1.",
            "inputSchema": obj(json!({}), &[]),
        }),
        json!({
            "name": "list_locations",
            "description": "The persisted Fable Files locations (from locations.json) with their root paths and whether each root is present right now.",
            "inputSchema": obj(json!({}), &[]),
        }),
        json!({
            "name": "list_volumes",
            "description": "Every volume Library Memory knows (filesystem UUID, label, first/last seen, last indexed, mount history) and whether it is connected right now.",
            "inputSchema": obj(json!({}), &[]),
        }),
        json!({
            "name": "list_collections",
            "description": "All collections with kind and live member counts (including how many members no longer resolve to a catalog item).",
            "inputSchema": obj(json!({}), &[]),
        }),
        json!({
            "name": "collection_items",
            "description": "The items of one collection by collection_id (from list_collections), plus the member paths that no longer resolve.",
            "inputSchema": obj(json!({
                "collection_id": { "type": "string" },
            }), &["collection_id"]),
        }),
        json!({
            "name": "list_jobs",
            "description": "Recent jobs from the catalog's durable job table (the executor checkpoints every job there): title, state, step and byte progress. Newest first.",
            "inputSchema": obj(json!({
                "limit": { "type": "integer", "description": "Max jobs (default 20, max 100)." },
            }), &[]),
        }),
    ]
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn req_str(args: &Value, key: &str) -> Result<String, String> {
    str_arg(args, key)
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing required string argument: {key}"))
}

fn u64_arg(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
}

// ---------------------------------------------------------------------------
// Item shaping — mirrors commands::finish_search hydration
// ---------------------------------------------------------------------------

fn item_json(
    item: &ufm_vfs::Item,
    mounted: &[commands::MountedVolume],
    volumes: &[ufm_catalog::VolumeRecord],
) -> Value {
    let badge = item
        .provider_key
        .as_deref()
        .and_then(|pk| commands::volume_badge_for_path(mounted, volumes, pk));
    json!({
        "item_id": item.id.to_string(),
        "name": item.name,
        "kind": format!("{:?}", item.kind).to_lowercase(),
        "path": item.provider_key,
        "size_bytes": item.size_bytes,
        "modified": item.modified.map(|m| m.to_rfc3339()),
        "labels": item.labels,
        "starred": item.starred,
        "volume_label": badge.as_ref().map(|b| b.label.clone()),
        "volume_offline": badge.as_ref().is_some_and(|b| b.offline),
        "volume_last_seen": badge.and_then(|b| b.last_seen),
    })
}

/// Hydrate labels + stars onto rows (the FTS rows carry neither) and
/// shape them with volume badges — the shared tail of search and
/// collection_items, mirroring `commands::finish_search`.
fn finish_items(catalog: &Catalog, mut rows: Vec<ufm_vfs::Item>) -> Vec<Value> {
    let keys: Vec<String> = rows
        .iter()
        .filter_map(|i| i.provider_key.clone())
        .collect();
    let starred = catalog.starred_paths(&keys).unwrap_or_default();
    for item in rows.iter_mut() {
        if let Some(pk) = &item.provider_key {
            item.labels = catalog.labels_for_path(pk).unwrap_or_default();
            item.starred = starred.contains(pk);
        }
    }
    let mounted = commands::current_filesystems();
    let volumes = catalog.volumes().unwrap_or_default();
    rows.iter()
        .map(|item| item_json(item, &mounted, &volumes))
        .collect()
}

// ---------------------------------------------------------------------------
// Tool handlers — real machinery only
// ---------------------------------------------------------------------------

fn tool_search(catalog: &Catalog, args: &Value) -> Result<Value, String> {
    let query_text = str_arg(args, "query").unwrap_or_default().to_string();
    let limit = u64_arg(args, "limit", 50).clamp(1, 500) as u32;
    let path_prefix = str_arg(args, "path_prefix").map(|s| s.to_string());
    let exts: Option<&'static [&'static str]> =
        str_arg(args, "type_filter").and_then(commands::type_extensions);

    let apply_prefix = |items: Vec<ufm_vfs::Item>| -> Vec<ufm_vfs::Item> {
        match &path_prefix {
            None => items,
            Some(p) => items
                .into_iter()
                .filter(|i| {
                    i.provider_key
                        .as_deref()
                        .map(|k| k.starts_with(p.as_str()))
                        .unwrap_or(false)
                })
                .collect(),
        }
    };

    // Type-only browsing (no text): matching files by recency — the
    // same branch commands::search takes.
    if query_text.trim().is_empty() {
        if let Some(exts) = exts {
            let rows = catalog
                .browse_by_extensions(exts, limit)
                .map_err(|e| e.to_string())?;
            let items = apply_prefix(rows);
            let hits = finish_items(catalog, items);
            return Ok(json!({ "count": hits.len(), "hits": hits }));
        }
        return Ok(json!({ "count": 0, "hits": [] }));
    }

    let query = ufm_search::parse(&query_text)
        .map_err(|e| format!("could not parse query {query_text:?}: {e}"))?;
    let (label_facets, starred_facet) = ufm_search::metadata_facets(&query);
    let book_facets = ufm_search::book_facets(&query);
    let has_metadata =
        !label_facets.is_empty() || starred_facet.is_some() || !book_facets.is_empty();
    let fts_query = ufm_search::to_fts(&query);

    let mut items: Vec<ufm_vfs::Item> = if fts_query.trim().is_empty() {
        if !has_metadata {
            return Ok(json!({ "count": 0, "hits": [] }));
        }
        // Metadata-only query: seed from the label/star/book stores.
        let mut seeded = if let Some(first) = label_facets.first() {
            catalog.items_by_label(first).map_err(|e| e.to_string())?
        } else if starred_facet.is_some() {
            catalog.starred_items().map_err(|e| e.to_string())?
        } else {
            commands::book_seed_items(catalog, &book_facets).map_err(|e| e.to_string())?
        };
        seeded.retain(|item| {
            commands::metadata_match(catalog, item, &label_facets, starred_facet)
                && commands::book_match(catalog, item, &book_facets)
        });
        if let Some(exts) = exts {
            seeded.retain(|item| {
                let lower = item.name.to_lowercase();
                exts.iter().any(|e| lower.ends_with(&format!(".{e}")))
            });
        }
        seeded.truncate(limit as usize);
        seeded
    } else {
        // With a type filter, over-fetch so filtering still fills
        // the page (same trick as the GUI command).
        let fetch = if exts.is_some() {
            (limit * 8).min(400)
        } else {
            limit
        };
        let mut rows = catalog
            .search_fts(&fts_query, fetch)
            .map_err(|e| e.to_string())?;
        if let Some(exts) = exts {
            rows.retain(|(item, _)| {
                let lower = item.name.to_lowercase();
                exts.iter().any(|e| lower.ends_with(&format!(".{e}")))
            });
            rows.truncate(limit as usize);
        }
        if has_metadata {
            rows.retain(|(item, _)| {
                commands::metadata_match(catalog, item, &label_facets, starred_facet)
                    && commands::book_match(catalog, item, &book_facets)
            });
        }
        rows.into_iter().map(|(item, _)| item).collect()
    };
    items = apply_prefix(items);
    let hits = finish_items(catalog, items);
    Ok(json!({ "count": hits.len(), "hits": hits }))
}

fn resolve_item(catalog: &Catalog, args: &Value) -> Result<ufm_vfs::Item, String> {
    if let Some(raw) = str_arg(args, "item_id") {
        let uuid = uuid::Uuid::parse_str(raw)
            .map_err(|e| format!("item_id is not a UUID: {e}"))?;
        return catalog
            .item_by_id(&ItemId(uuid))
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no catalog item with id {raw}"));
    }
    if let Some(path) = str_arg(args, "path") {
        return catalog
            .item_by_path(path)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no catalog item at path {path}"));
    }
    Err("get_metadata/read_item need item_id or path".into())
}

fn tool_get_metadata(catalog: &Catalog, args: &Value) -> Result<Value, String> {
    let mut item = resolve_item(catalog, args)?;
    if let Some(pk) = &item.provider_key {
        item.labels = catalog.labels_for_path(pk).unwrap_or_default();
        item.starred = catalog.is_starred_path(pk).unwrap_or(false);
    }
    let mounted = commands::current_filesystems();
    let volumes = catalog.volumes().unwrap_or_default();
    let mut out = item_json(&item, &mounted, &volumes);
    let identity = catalog
        .media_identity_for(&item.id)
        .map_err(|e| e.to_string())?;
    out["media_identity"] = match identity {
        Some(mi) => json!({
            "media_kind": mi.media_kind,
            "title": mi.title,
            "platform": mi.platform,
            "sha1": mi.sha1,
        }),
        None => Value::Null,
    };
    Ok(out)
}

fn tool_read_item(catalog: &Catalog, args: &Value) -> Result<Value, String> {
    let item = resolve_item(catalog, args)?;
    if item.kind != ufm_vfs::ItemKind::File {
        return Err(format!("item {} is a {:?}, not a file", item.name, item.kind));
    }
    let Some(path) = item.provider_key.clone() else {
        return Err("item has no local path to read".into());
    };
    let meta = std::fs::metadata(&path)
        .map_err(|e| format!("cannot read {path}: {e} — is the drive connected?"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a regular file"));
    }
    let offset = u64_arg(args, "offset", 0);
    let length = u64_arg(args, "length", READ_CAP).min(READ_CAP);
    let mut file =
        std::fs::File::open(&path).map_err(|e| format!("cannot open {path}: {e}"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("cannot seek {path}: {e}"))?;
    let mut buf = Vec::new();
    file.take(length)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read {path}: {e}"))?;
    // More file beyond what this call returned?
    let more_remains = offset + (buf.len() as u64) < meta.len();
    let (encoding, data) = match std::str::from_utf8(&buf) {
        Ok(text) => ("utf-8", text.to_string()),
        Err(_) => ("base64", base64_encode(&buf)),
    };
    Ok(json!({
        "item_id": item.id.to_string(),
        "path": path,
        "offset": offset,
        "bytes_read": buf.len(),
        "size_bytes": meta.len(),
        "truncated": more_remains,
        "encoding": encoding,
        "data": data,
    }))
}

fn tool_tag(catalog: &Catalog, args: &Value) -> Result<Value, String> {
    let path = req_str(args, "path")?;
    let label = req_str(args, "label")?;
    let action = str_arg(args, "action").unwrap_or("add");
    match action {
        "add" => catalog.tag_path(&path, &label),
        "remove" => catalog.untag_path(&path, &label),
        other => return Err(format!("unknown tag action {other:?} — use add or remove")),
    }
    .map_err(|e| e.to_string())?;
    Ok(json!({
        "path": path,
        "label": label,
        "action": action,
        "labels": catalog.labels_for_path(&path).map_err(|e| e.to_string())?,
    }))
}

fn tool_dedupe(catalog: &Catalog, args: &Value) -> Result<Value, String> {
    let limit = u64_arg(args, "limit", 20).clamp(1, 100) as u32;
    let groups = catalog
        .duplicate_candidates(limit)
        .map_err(|e| e.to_string())?;
    let groups: Vec<Value> = groups
        .iter()
        .map(|g| {
            json!({
                "size_bytes": g.size_bytes,
                "count": g.count,
                "wasted_bytes_upper_bound": g.wasted_bytes,
                "sample_paths": g.sample_paths,
            })
        })
        .collect();
    Ok(json!({
        "note": "Candidates by identical byte size only — content is NOT hashed, \
                 so equal size does not prove equal content. Report only; \
                 this tool never deletes anything.",
        "groups": groups,
    }))
}

fn tool_integrity(catalog: &Catalog) -> Result<Value, String> {
    Ok(json!({
        "integrity": catalog.integrity_check().map_err(|e| e.to_string())?,
        "locations": catalog.location_count().map_err(|e| e.to_string())?,
        "items": catalog.item_count().map_err(|e| e.to_string())?,
        "volumes_remembered": catalog.volumes().map_err(|e| e.to_string())?.len(),
        "database_size_bytes": catalog.database_size_bytes().map_err(|e| e.to_string())?,
    }))
}

#[derive(serde::Deserialize)]
struct PersistedLocation {
    id: String,
    root_item: String,
    provider: String,
    display_name: String,
    root: String,
}

fn tool_list_locations(base: &Path) -> Result<Value, String> {
    let path = base.join("locations.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(json!({ "count": 0, "locations": [] })),
    };
    let rows: Vec<PersistedLocation> =
        serde_json::from_str(&text).map_err(|e| format!("locations.json: {e}"))?;
    let locations: Vec<Value> = rows
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "root_item": l.root_item,
                "provider": l.provider,
                "display_name": l.display_name,
                "root": l.root,
                "online": Path::new(&l.root).is_dir(),
            })
        })
        .collect();
    Ok(json!({ "count": locations.len(), "locations": locations }))
}

fn tool_list_volumes(catalog: &Catalog) -> Result<Value, String> {
    let mounted = commands::current_filesystems();
    let records = catalog.volumes().map_err(|e| e.to_string())?;
    let volumes: Vec<Value> = records
        .iter()
        .map(|r| {
            let live = mounted
                .iter()
                .find(|m| m.uuid.as_deref() == Some(r.fs_uuid.as_str()));
            json!({
                "fs_uuid": r.fs_uuid,
                "label": r.label,
                "first_seen": r.first_seen,
                "last_seen": r.last_seen,
                "last_indexed": r.last_indexed,
                "online": live.is_some(),
                "current_path": live.map(|m| m.path.clone()),
                "mount_history": r.mount_history.iter().map(|s| json!({
                    "path": s.path,
                    "first_seen": s.first_seen,
                    "last_seen": s.last_seen,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({ "count": volumes.len(), "volumes": volumes }))
}

fn tool_list_collections(catalog: &Catalog) -> Result<Value, String> {
    let rows = catalog.list_collections().map_err(|e| e.to_string())?;
    let collections: Vec<Value> = rows
        .iter()
        .map(|c| {
            json!({
                "collection_id": c.id,
                "name": c.name,
                "kind": c.kind,
                "member_count": c.member_count,
                "missing_count": c.missing_count,
            })
        })
        .collect();
    Ok(json!({ "count": collections.len(), "collections": collections }))
}

fn tool_collection_items(catalog: &Catalog, args: &Value) -> Result<Value, String> {
    let raw = req_str(args, "collection_id")?;
    let uuid = uuid::Uuid::parse_str(&raw)
        .map_err(|e| format!("collection_id is not a UUID: {e}"))?;
    let (items, missing) = catalog
        .collection_items(&CollectionId(uuid))
        .map_err(|e| e.to_string())?;
    let hits = finish_items(catalog, items);
    Ok(json!({
        "collection_id": raw,
        "count": hits.len(),
        "items": hits,
        "missing_paths": missing,
    }))
}

fn tool_list_jobs(catalog: &Catalog, args: &Value) -> Result<Value, String> {
    let limit = u64_arg(args, "limit", 20).clamp(1, 100) as u32;
    let jobs = catalog.recent_jobs(limit).map_err(|e| e.to_string())?;
    let jobs: Vec<Value> = jobs
        .iter()
        .map(|j| {
            // Same truthfulness rules as the GUI's list_jobs: slimmed
            // snapshots report from their step tallies.
            let (steps_done, steps_total) = if j.steps.is_empty() {
                match &j.step_counts {
                    Some(c) => ((c.done + c.skipped) as usize, c.total as usize),
                    None => (0, 0),
                }
            } else {
                (
                    j.steps
                        .values()
                        .filter(|s| {
                            matches!(
                                s.state,
                                ufm_jobs::StepState::Done | ufm_jobs::StepState::Skipped
                            )
                        })
                        .count(),
                    j.steps.len(),
                )
            };
            json!({
                "job_id": j.id.to_string(),
                "title": j.title,
                "state": commands::job_state_name(j.state),
                "created": j.created.to_rfc3339(),
                "steps_done": steps_done,
                "steps_total": steps_total,
                "bytes_done": commands::job_bytes_done(j),
                "bytes_total": j.preview.bytes_to_move,
                "packed": j.packed,
            })
        })
        .collect();
    Ok(json!({ "count": jobs.len(), "jobs": jobs }))
}

// ---------------------------------------------------------------------------
// Base64 (RFC 4648, padded) — read_item's binary fallback. Hand-rolled
// so the stdio path needs no extra dependency.
// ---------------------------------------------------------------------------

fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0] as u32,
            *chunk.get(1).unwrap_or(&0) as u32,
            *chunk.get(2).unwrap_or(&0) as u32,
        ];
        let n = (b[0] << 16) | (b[1] << 8) | b[2];
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------------
// In-app read path (docs/MCP_API.md: the audit chain is "queryable in
// the app"). No UI chrome ships with this command in v1 — it is the
// backend half the future Activity surface will call.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct McpAuditView {
    pub seq: i64,
    pub at: String,
    pub client: String,
    pub tool: String,
    pub args_json: String,
    pub outcome: String,
    pub is_write: bool,
    pub duration_ms: i64,
}

#[tauri::command]
pub fn mcp_audit_recent(
    state: tauri::State<'_, crate::AppState>,
    limit: Option<u32>,
) -> Result<Vec<McpAuditView>, String> {
    let catalog = state.catalog.lock().unwrap();
    let rows = catalog
        .recent_mcp_audit(limit.unwrap_or(50).min(500))
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|r| McpAuditView {
            seq: r.seq,
            at: r.at,
            client: r.client,
            tool: r.tool,
            args_json: r.args_json,
            outcome: r.outcome,
            is_write: r.is_write,
            duration_ms: r.duration_ms,
        })
        .collect())
}
