//! WASM plugin runtime (Phase 6, spec §§17–18).
//!
//! Executes the `entry` module of a discovered plugin under the
//! manifest's [`SandboxPolicy`]. The runtime choice is **wasmi**, a
//! pure-Rust interpreter: fuel metering turns the spec's per-call CPU
//! budget into a hard, testable limit, the store limiter enforces the
//! memory budget, and there is no native code generation surface.
//!
//! There is deliberately **no WASI**. A plugin can touch nothing the
//! host does not hand it through the `fable` import module:
//!
//! - `fable.log(ptr, len)` — append a line to the call's log.
//! - `fable.read_item(path_ptr, path_len, out_ptr, out_cap) -> i32` —
//!   write the invoked item's metadata JSON into guest memory.
//!   Answers **only** for the item the plugin was invoked on
//!   (invocation scope, enforced here); anything else returns -1,
//!   a too-small buffer returns -2.
//!
//! Guest contract: exports `memory`, `alloc(size) -> ptr`,
//! `dealloc(ptr, size)`, and `fable_event(ptr, len) -> u64`
//! (packed `ptr << 32 | len`). Input and output are JSON:
//!
//! in:  `{"type":"action","action":"context","item":{…metadata…}}`
//! out: `{"logs":[…], "proposals":[{"kind":"create_text_file",
//!        "name":"x.txt","contents":"…"}]}`
//!
//! A plugin may only *propose* filesystem effects. Proposals are
//! validated here (kind allowlist, file-name sanitation, size cap)
//! and returned to the shell, which asks the user and — on confirm —
//! runs them through the journaled job engine. Nothing in this module
//! writes to the filesystem.
//!
//! A fresh instance is created per call: no plugin state survives an
//! invocation, and a trapped plugin can never wedge the host.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use ufm_core::{Result, UfmError};
use wasmi::{Caller, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

use crate::SandboxPolicy;

/// Capabilities the host broker can actually dispatch in v1.
pub const CAPABILITY_ACTION_CONTEXT: &str = "action.context";

/// Fuel granted per policy CPU-second. Fuel is an abstract budget
/// (roughly one unit per executed instruction); a metadata-scale
/// plugin uses a tiny fraction of one second's worth.
const FUEL_PER_CPU_SECOND: u64 = 10_000_000;
/// Host-side caps on what a call may hand back.
const MAX_LOG_LINES: usize = 50;
const MAX_LOG_LINE_BYTES: usize = 4_096;
const MAX_PROPOSALS: usize = 16;
/// The WriteFile job-step contract (the planner enforces the same).
pub const MAX_PROPOSAL_CONTENTS_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Manifest — parsed leniently from the DOCUMENTED shape (plugins/README:
// a `[plugin]` table; every field optional except the table itself).
// The scaffold `PluginManifest` demands a UUID `id` the docs never
// mention, so manifests written per the docs fail against it; the
// runtime's identity for a plugin is its folder name instead.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeManifest {
    pub name: String,
    pub author: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    pub entry: String,
}

impl RuntimeManifest {
    pub fn claims(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

fn toml_str(table: &toml::Value, key: &str) -> Option<String> {
    table.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn toml_str_list(table: &toml::Value, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

pub fn load_runtime_manifest(dir: &Path) -> Result<RuntimeManifest> {
    let text = std::fs::read_to_string(dir.join("plugin.toml"))
        .map_err(|e| UfmError::Plugin(format!("cannot read plugin.toml: {e}")))?;
    let doc: toml::Value = text
        .parse::<toml::Value>()
        .map_err(|e| UfmError::Plugin(format!("bad plugin.toml: {e}")))?;
    // Documented shape nests fields under [plugin]; accept a flat
    // table too (the scaffold's shape) rather than failing either.
    let table = doc.get("plugin").cloned().unwrap_or(doc);
    let folder = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "plugin".into());
    Ok(RuntimeManifest {
        name: toml_str(&table, "name").unwrap_or(folder),
        author: toml_str(&table, "author").unwrap_or_default(),
        version: toml_str(&table, "version").unwrap_or_else(|| "0.0.0".into()),
        description: toml_str(&table, "description").unwrap_or_default(),
        capabilities: toml_str_list(&table, "capabilities"),
        permissions: toml_str_list(&table, "permissions"),
        entry: toml_str(&table, "entry").unwrap_or_else(|| "main.wasm".into()),
    })
}

// ---------------------------------------------------------------------------
// Discovery — one bad plugin never takes down the host (QA checklist):
// invalid manifests surface as entries with an error, not as a failure
// of the whole scan.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// Folder name — the plugin's stable identity.
    pub key: String,
    pub dir: PathBuf,
    pub manifest: Option<RuntimeManifest>,
    pub error: Option<String>,
}

pub fn discover_plugins(root: &Path) -> Vec<DiscoveredPlugin> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || !dir.join("plugin.toml").exists() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().into_owned();
        match load_runtime_manifest(&dir) {
            Ok(manifest) => out.push(DiscoveredPlugin {
                key,
                dir,
                manifest: Some(manifest),
                error: None,
            }),
            Err(e) => out.push(DiscoveredPlugin {
                key,
                dir,
                manifest: None,
                error: Some(e.to_string()),
            }),
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

// ---------------------------------------------------------------------------
// Invocation.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub kind: String,
    pub name: String,
    pub contents: String,
}

#[derive(Debug, Clone, Default)]
pub struct PluginOutcome {
    pub logs: Vec<String>,
    pub proposals: Vec<Proposal>,
}

#[derive(Debug, Deserialize, Default)]
struct GuestResponse {
    #[serde(default)]
    logs: Vec<String>,
    #[serde(default)]
    proposals: Vec<GuestProposal>,
}

#[derive(Debug, Deserialize)]
struct GuestProposal {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    contents: String,
}

struct HostState {
    /// The one path `read_item` may answer for.
    scope_path: String,
    /// Pre-resolved metadata JSON for the scoped item.
    item_json: String,
    logs: Vec<String>,
    limits: StoreLimits,
}

fn guest_memory(caller: &Caller<'_, HostState>) -> Option<wasmi::Memory> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
}

fn host_log(mut caller: Caller<'_, HostState>, ptr: u32, len: u32) -> std::result::Result<(), wasmi::Error> {
    let Some(mem) = guest_memory(&caller) else {
        return Ok(());
    };
    let mut buf = vec![0u8; (len as usize).min(MAX_LOG_LINE_BYTES)];
    if mem.read(&caller, ptr as usize, &mut buf).is_ok() {
        let line = String::from_utf8_lossy(&buf).into_owned();
        let state = caller.data_mut();
        if state.logs.len() < MAX_LOG_LINES {
            state.logs.push(line);
        }
    }
    Ok(())
}

fn host_read_item(
    mut caller: Caller<'_, HostState>,
    path_ptr: u32,
    path_len: u32,
    out_ptr: u32,
    out_cap: u32,
) -> std::result::Result<i32, wasmi::Error> {
    let Some(mem) = guest_memory(&caller) else {
        return Ok(-1);
    };
    let mut buf = vec![0u8; (path_len as usize).min(8_192)];
    if mem.read(&caller, path_ptr as usize, &mut buf).is_err() {
        return Ok(-1);
    }
    let asked = String::from_utf8_lossy(&buf).into_owned();
    // Clone the payload out before writing (borrow discipline), and
    // enforce invocation scope: only the item this call is about.
    let payload: Option<Vec<u8>> = {
        let state = caller.data();
        if asked == state.scope_path && !state.item_json.is_empty() {
            Some(state.item_json.clone().into_bytes())
        } else {
            None
        }
    };
    let Some(payload) = payload else {
        return Ok(-1);
    };
    if payload.len() > out_cap as usize {
        return Ok(-2);
    }
    match mem.write(&mut caller, out_ptr as usize, &payload) {
        Ok(()) => Ok(payload.len() as i32),
        Err(_) => Ok(-1),
    }
}

/// The proposal safety rules, shared by the runtime (which drops
/// unsafe guest proposals) and the shell's confirm path (which
/// re-checks a proposal before it becomes a job — the UI round trip
/// is never trusted). `Err` is a plain-language reason.
pub fn check_proposal(name: &str, contents: &str) -> std::result::Result<(), String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 128
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
    {
        return Err(format!("unsafe file name {name:?}"));
    }
    if contents.len() > MAX_PROPOSAL_CONTENTS_BYTES {
        return Err(format!(
            "contents too large ({} bytes, cap {})",
            contents.len(),
            MAX_PROPOSAL_CONTENTS_BYTES
        ));
    }
    Ok(())
}

/// Validate one guest proposal. `Err` carries the plain-language
/// reason it was dropped (surfaced in the call's log, never silent).
fn validate_proposal(raw: GuestProposal) -> std::result::Result<Proposal, String> {
    if raw.kind != "create_text_file" {
        return Err(format!("unknown proposal kind {:?}", raw.kind));
    }
    check_proposal(&raw.name, &raw.contents)?;
    Ok(Proposal {
        kind: raw.kind,
        name: raw.name.trim().to_string(),
        contents: raw.contents,
    })
}

/// Run one event through the plugin in `dir`. `scope_path` +
/// `item_json` are the invoked item (resolved by the shell from the
/// catalog); the event kind/action ride in the JSON envelope.
pub fn invoke_plugin(
    dir: &Path,
    manifest: &RuntimeManifest,
    policy: &SandboxPolicy,
    scope_path: &str,
    item_json: &str,
    event_type: &str,
    action: &str,
) -> Result<PluginOutcome> {
    if !manifest.claims(CAPABILITY_ACTION_CONTEXT) {
        return Err(UfmError::Plugin(format!(
            "plugin {:?} does not declare the {CAPABILITY_ACTION_CONTEXT} capability",
            manifest.name
        )));
    }
    // The entry must be a bare file name inside the plugin folder —
    // no traversal out of the sandbox directory.
    if manifest.entry.is_empty()
        || manifest.entry.contains('/')
        || manifest.entry.contains('\\')
        || manifest.entry == ".."
    {
        return Err(UfmError::Plugin(format!(
            "plugin entry {:?} is not a plain file name",
            manifest.entry
        )));
    }
    let wasm = std::fs::read(dir.join(&manifest.entry))
        .map_err(|e| UfmError::Plugin(format!("cannot read plugin entry: {e}")))?;

    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, &wasm[..])
        .map_err(|e| UfmError::Plugin(format!("plugin module does not compile: {e}")))?;

    let limits = StoreLimitsBuilder::new()
        .memory_size((policy.max_memory_mb.max(1) * 1024 * 1024) as usize)
        .build();
    let mut store = Store::new(
        &engine,
        HostState {
            scope_path: scope_path.to_string(),
            item_json: item_json.to_string(),
            logs: Vec::new(),
            limits,
        },
    );
    store.limiter(|state| &mut state.limits);
    let fuel = policy
        .max_cpu_seconds
        .max(1)
        .saturating_mul(FUEL_PER_CPU_SECOND);
    let _ = store.set_fuel(fuel);

    let mut linker = Linker::<HostState>::new(&engine);
    linker
        .func_wrap("fable", "log", host_log)
        .map_err(|e| UfmError::Plugin(format!("host link failed: {e}")))?;
    linker
        .func_wrap("fable", "read_item", host_read_item)
        .map_err(|e| UfmError::Plugin(format!("host link failed: {e}")))?;
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(|e| UfmError::Plugin(format!("plugin failed to start: {e}")))?;

    let memory = instance
        .get_memory(&store, "memory")
        .ok_or_else(|| UfmError::Plugin("plugin does not export memory".into()))?;
    let alloc = instance
        .get_typed_func::<u32, u32>(&store, "alloc")
        .map_err(|_| UfmError::Plugin("plugin does not export alloc(size)".into()))?;
    let fable_event = instance
        .get_typed_func::<(u32, u32), u64>(&store, "fable_event")
        .map_err(|_| UfmError::Plugin("plugin does not export fable_event(ptr, len)".into()))?;

    let item_value: serde_json::Value =
        serde_json::from_str(item_json).unwrap_or(serde_json::Value::Null);
    let event = serde_json::json!({
        "type": event_type,
        "action": action,
        "item": item_value,
    })
    .to_string();
    let event_bytes = event.as_bytes();
    let in_ptr = alloc
        .call(&mut store, event_bytes.len() as u32)
        .map_err(|e| UfmError::Plugin(format!("plugin alloc failed: {e}")))?;
    memory
        .write(&mut store, in_ptr as usize, event_bytes)
        .map_err(|e| UfmError::Plugin(format!("cannot pass event to plugin: {e}")))?;
    let packed = fable_event
        .call(&mut store, (in_ptr, event_bytes.len() as u32))
        .map_err(|e| UfmError::Plugin(format!("plugin call failed: {e}")))?;
    let out_ptr = (packed >> 32) as u32;
    let out_len = (packed & 0xFFFF_FFFF) as u32;
    if out_len as usize > MAX_PROPOSAL_CONTENTS_BYTES * 4 {
        return Err(UfmError::Plugin("plugin returned an oversized response".into()));
    }
    let mut out_buf = vec![0u8; out_len as usize];
    memory
        .read(&store, out_ptr as usize, &mut out_buf)
        .map_err(|e| UfmError::Plugin(format!("cannot read plugin response: {e}")))?;

    let mut outcome = PluginOutcome {
        logs: std::mem::take(&mut store.data_mut().logs),
        proposals: Vec::new(),
    };
    let response_text = String::from_utf8_lossy(&out_buf).into_owned();
    let parsed: GuestResponse = serde_json::from_str(&response_text).unwrap_or_else(|_| {
        // A plugin that answers non-JSON has said nothing actionable;
        // keep its raw words in the log rather than failing the call.
        if !response_text.trim().is_empty() {
            outcome.logs.push(response_text.trim().to_string());
        }
        GuestResponse::default()
    });
    outcome.logs.extend(parsed.logs);
    outcome.logs.truncate(MAX_LOG_LINES);
    for raw in parsed.proposals.into_iter().take(MAX_PROPOSALS) {
        match validate_proposal(raw) {
            Ok(p) => outcome.proposals.push(p),
            Err(why) => outcome.logs.push(format!("Dropped a proposal: {why}.")),
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SandboxPolicy {
        SandboxPolicy::locked_down()
    }

    fn manifest() -> RuntimeManifest {
        RuntimeManifest {
            name: "Test Plugin".into(),
            author: "tests".into(),
            version: "0.1.0".into(),
            description: "".into(),
            capabilities: vec![CAPABILITY_ACTION_CONTEXT.into()],
            permissions: vec![],
            entry: "main.wasm".into(),
        }
    }

    /// Write a WAT guest as main.wasm in a temp plugin dir.
    fn plugin_dir(wat: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("test-plugin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.wasm"), wat.as_bytes()).unwrap();
        (tmp, dir)
    }

    // Guest prelude: memory + a bump allocator starting clear of the
    // data segments (which live below 8 KiB).
    const PRELUDE: &str = r#"
        (memory (export "memory") 2)
        (global $bump (mut i32) (i32.const 8192))
        (func (export "alloc") (param $n i32) (result i32)
            (local $p i32)
            (local.set $p (global.get $bump))
            (global.set $bump (i32.add (global.get $bump) (local.get $n)))
            (local.get $p))
        (func (export "dealloc") (param i32 i32))
    "#;

    fn packed(ptr: i32, len: i32) -> String {
        format!(
            "(i64.or (i64.shl (i64.extend_i32_u (i32.const {ptr})) (i64.const 32)) \
             (i64.extend_i32_u (i32.const {len})))"
        )
    }

    #[test]
    fn documented_manifest_shape_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("my-plugin");
        std::fs::create_dir_all(&dir).unwrap();
        // Exactly the plugins/README example (no `id` field).
        std::fs::write(
            dir.join("plugin.toml"),
            "[plugin]\nname = \"My Search Provider\"\nauthor = \"you\"\nversion = \"0.1.0\"\n\
             description = \"Adds an extra search provider.\"\n\
             capabilities = [\"search.provider\"]\npermissions = []\nentry = \"main.wasm\"\n",
        )
        .unwrap();
        let m = load_runtime_manifest(&dir).unwrap();
        assert_eq!(m.name, "My Search Provider");
        assert!(m.claims("search.provider"));
        assert_eq!(m.entry, "main.wasm");
    }

    #[test]
    fn discovery_isolates_a_bad_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("plugin.toml"), "[plugin]\nname=\"G\"\n").unwrap();
        let bad = tmp.path().join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("plugin.toml"), "this is [ not toml").unwrap();
        let found = discover_plugins(tmp.path());
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|p| p.key == "good" && p.error.is_none()));
        assert!(found.iter().any(|p| p.key == "bad" && p.error.is_some()));
    }

    #[test]
    fn happy_path_logs_and_proposal() {
        // Logs via the host function, then returns a static JSON
        // response carrying one valid proposal.
        let response = r#"{"logs":["response log"],"proposals":[{"kind":"create_text_file","name":"note.txt","contents":"hello"}]}"#;
        let response_wat = response.replace('"', "\\\"");
        let wat = format!(
            r#"(module
                (import "fable" "log" (func $log (param i32 i32)))
                (import "fable" "read_item" (func $read (param i32 i32 i32 i32) (result i32)))
                {PRELUDE}
                (data (i32.const 0) "hello from wasm")
                (data (i32.const 1024) "{response_wat}")
                (func (export "fable_event") (param i32 i32) (result i64)
                    (call $log (i32.const 0) (i32.const 15))
                    {packed}
                )
            )"#,
            packed = packed(1024, response.len() as i32),
        );
        let (_tmp, dir) = plugin_dir(&wat);
        let out = invoke_plugin(
            &dir,
            &manifest(),
            &policy(),
            "/books/test.epub",
            r#"{"path":"/books/test.epub","name":"test.epub"}"#,
            "action",
            "context",
        )
        .unwrap();
        assert!(out.logs.iter().any(|l| l == "hello from wasm"), "logs: {:?}", out.logs);
        assert!(out.logs.iter().any(|l| l == "response log"));
        assert_eq!(out.proposals.len(), 1);
        assert_eq!(out.proposals[0].name, "note.txt");
        assert_eq!(out.proposals[0].contents, "hello");
    }

    #[test]
    fn read_item_answers_only_for_the_invoked_item() {
        // Guest reads a hardcoded path and reports the verdict by log.
        let guest = |path: &str| {
            format!(
                r#"(module
                    (import "fable" "log" (func $log (param i32 i32)))
                    (import "fable" "read_item" (func $read (param i32 i32 i32 i32) (result i32)))
                    {PRELUDE}
                    (data (i32.const 0) "{path}")
                    (data (i32.const 1024) "read ok")
                    (data (i32.const 2048) "read denied")
                    (data (i32.const 4096) "{{}}")
                    (func (export "fable_event") (param i32 i32) (result i64)
                        (local $n i32)
                        (local.set $n (call $read (i32.const 0) (i32.const {plen}) (i32.const 3072) (i32.const 512)))
                        (if (i32.ge_s (local.get $n) (i32.const 0))
                            (then (call $log (i32.const 1024) (i32.const 7)))
                            (else (call $log (i32.const 2048) (i32.const 11))))
                        {packed}
                    )
                )"#,
                plen = path.len(),
                packed = packed(4096, 2),
            )
        };
        let (_t1, dir_ok) = plugin_dir(&guest("/books/test.epub"));
        let out = invoke_plugin(
            &dir_ok,
            &manifest(),
            &policy(),
            "/books/test.epub",
            r#"{"path":"/books/test.epub"}"#,
            "action",
            "context",
        )
        .unwrap();
        assert!(out.logs.iter().any(|l| l == "read ok"), "logs: {:?}", out.logs);

        let (_t2, dir_no) = plugin_dir(&guest("/etc/passwd"));
        let out = invoke_plugin(
            &dir_no,
            &manifest(),
            &policy(),
            "/books/test.epub",
            r#"{"path":"/books/test.epub"}"#,
            "action",
            "context",
        )
        .unwrap();
        assert!(out.logs.iter().any(|l| l == "read denied"), "logs: {:?}", out.logs);
    }

    #[test]
    fn fuel_exhaustion_stops_an_infinite_loop() {
        let wat = format!(
            r#"(module
                (import "fable" "log" (func $log (param i32 i32)))
                (import "fable" "read_item" (func $read (param i32 i32 i32 i32) (result i32)))
                {PRELUDE}
                (func (export "fable_event") (param i32 i32) (result i64)
                    (loop $l (br $l))
                    (i64.const 0))
            )"#
        );
        let (_tmp, dir) = plugin_dir(&wat);
        let mut p = policy();
        p.max_cpu_seconds = 1; // 10M fuel — an empty loop burns it fast
        let err = invoke_plugin(&dir, &manifest(), &p, "/x", "{}", "action", "context")
            .unwrap_err();
        assert!(
            err.to_string().contains("plugin call failed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn memory_limit_stops_a_growth_bomb() {
        let wat = format!(
            r#"(module
                (import "fable" "log" (func $log (param i32 i32)))
                (import "fable" "read_item" (func $read (param i32 i32 i32 i32) (result i32)))
                {PRELUDE}
                (func (export "fable_event") (param i32 i32) (result i64)
                    (drop (memory.grow (i32.const 100000)))
                    (i64.const 0))
            )"#
        );
        let (_tmp, dir) = plugin_dir(&wat);
        let mut p = policy();
        p.max_memory_mb = 1;
        // memory.grow failure returns -1 rather than trapping, so the
        // guest "succeeds" with an empty response — the rail that
        // matters is that the host survives and memory stayed capped.
        // Use a grow-or-trap guest to prove enforcement instead:
        let wat2 = format!(
            r#"(module
                (import "fable" "log" (func $log (param i32 i32)))
                (import "fable" "read_item" (func $read (param i32 i32 i32 i32) (result i32)))
                {PRELUDE}
                (func (export "fable_event") (param i32 i32) (result i64)
                    (if (i32.eq (memory.grow (i32.const 100000)) (i32.const -1))
                        (then (unreachable)))
                    (i64.const 0))
            )"#
        );
        let (_t2, dir2) = plugin_dir(&wat2);
        assert!(invoke_plugin(&dir2, &manifest(), &p, "/x", "{}", "action", "context").is_err());
        let _ = invoke_plugin(&dir, &manifest(), &p, "/x", "{}", "action", "context");
    }

    #[test]
    fn unsafe_proposals_are_dropped_with_a_reason() {
        let response = r#"{"logs":[],"proposals":[{"kind":"create_text_file","name":"../evil.txt","contents":"x"},{"kind":"delete_everything","name":"a","contents":""},{"kind":"create_text_file","name":"fine.txt","contents":"ok"}]}"#;
        let response_wat = response.replace('"', "\\\"");
        let wat = format!(
            r#"(module
                (import "fable" "log" (func $log (param i32 i32)))
                (import "fable" "read_item" (func $read (param i32 i32 i32 i32) (result i32)))
                {PRELUDE}
                (data (i32.const 1024) "{response_wat}")
                (func (export "fable_event") (param i32 i32) (result i64)
                    {packed}
                )
            )"#,
            packed = packed(1024, response.len() as i32),
        );
        let (_tmp, dir) = plugin_dir(&wat);
        let out = invoke_plugin(&dir, &manifest(), &policy(), "/x", "{}", "action", "context")
            .unwrap();
        assert_eq!(out.proposals.len(), 1);
        assert_eq!(out.proposals[0].name, "fine.txt");
        assert_eq!(
            out.logs.iter().filter(|l| l.starts_with("Dropped a proposal")).count(),
            2,
            "logs: {:?}",
            out.logs
        );
    }

    #[test]
    fn example_plugin_proposes_a_metadata_note() {
        // The REAL shipped example (plugins/metadata-note/main.wasm,
        // built from the in-repo source) driven end to end.
        let wasm = include_bytes!("../../../plugins/metadata-note/main.wasm");
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("metadata-note");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.wasm"), wasm).unwrap();
        let item = r#"{"path":"/books/test.epub","name":"test.epub","kind":"file","size_bytes":1234567,"modified":"2026-10-14T03:33:45Z","labels":["fiction","favorites"],"starred":true}"#;
        let out = invoke_plugin(
            &dir,
            &manifest(),
            &policy(),
            "/books/test.epub",
            item,
            "action",
            "context",
        )
        .unwrap();
        assert!(
            out.logs.iter().any(|l| l.contains("test.epub")),
            "logs: {:?}",
            out.logs
        );
        assert_eq!(out.proposals.len(), 1, "outcome: {out:?}");
        let p = &out.proposals[0];
        assert_eq!(p.name, "test.epub.fable-note.txt");
        assert!(p.contents.contains("Name: test.epub"), "note: {}", p.contents);
        assert!(p.contents.contains("Labels: fiction, favorites"), "note: {}", p.contents);
        assert!(p.contents.contains("Starred: yes"), "note: {}", p.contents);
        assert!(p.contents.contains("Size: 1234567 bytes"), "note: {}", p.contents);
    }

    #[test]
    fn undeclared_capability_is_refused_before_any_code_runs() {
        let (_tmp, dir) = plugin_dir("(module)");
        let mut m = manifest();
        m.capabilities = vec!["search.provider".into()];
        let err = invoke_plugin(&dir, &m, &policy(), "/x", "{}", "action", "context")
            .unwrap_err();
        assert!(err.to_string().contains("does not declare"), "{err}");
    }
}
