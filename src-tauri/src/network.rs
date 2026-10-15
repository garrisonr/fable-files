//! Network Locations — GVfs/GIO servers as first-class Locations.
//!
//! One mount mechanism for every scheme (smb, ftp, ftps, sftp/ssh,
//! dav, davs, nfs, afp): GIO mounts the server (gvfsd-<scheme> does
//! the protocol work), the mount's FUSE path under
//! /run/user/<uid>/gvfs becomes a Location root, and everything
//! downstream — browsing, preview, the journaled job engine, the
//! indexer — treats it exactly like a local volume, because all of
//! that machinery is path-based.
//!
//! Credentials: the user types them into Fable's own Connect dialog;
//! a GIO MountOperation answers the backend's questions from those
//! values. A password the user asked us to remember lives ONLY in
//! the OS keyring (service = the app id, entry `network/<uri>`) —
//! never in the bookmark sidecar, never inside a stored URL.

use gio::glib;
use gio::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// URI normalization (pure — the identity of a server).
// ---------------------------------------------------------------------------

pub const SUPPORTED_SCHEMES: &[&str] = &[
    "smb", "ftp", "ftps", "sftp", "dav", "davs", "nfs", "afp",
];

fn default_port(scheme: &str) -> Option<u16> {
    Some(match scheme {
        "smb" => 445,
        "ftp" | "ftps" => 21,
        "sftp" => 22,
        "dav" => 80,
        "davs" => 443,
        "afp" => 548,
        _ => return None,
    })
}

/// A server address, parsed and normalized. `uri` is the identity:
/// scheme://[user@]host[:port]/path — password-free by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerUri {
    pub uri: String,
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub path: String,
}

/// Parse + normalize a user-typed server address.
///
/// Rules: scheme lowercased (`ssh` → `sftp`); host lowercased; a
/// non-default port is kept, the scheme's default port is dropped
/// (identity stability: `sftp://h:22/x` and `sftp://h/x` are the same
/// server); a single trailing slash on the path is dropped (except
/// the root); a password embedded in the address is REJECTED with
/// plain language — it belongs in the password field, where it can
/// be kept in the keyring instead of sitting in history files.
pub fn normalize_server_uri(raw: &str) -> Result<ServerUri, String> {
    let raw = raw.trim();
    let (scheme_raw, rest) = raw
        .split_once("://")
        .ok_or_else(|| "That address needs a scheme — like sftp://server/share, smb://server/share, or dav://server/path.".to_string())?;
    let mut scheme = scheme_raw.trim().to_lowercase();
    if scheme == "ssh" {
        scheme = "sftp".to_string();
    }
    if !SUPPORTED_SCHEMES.contains(&scheme.as_str()) {
        return Err(format!(
            "“{scheme}://” isn't a server kind Fable can connect to. Supported: smb://, sftp:// (ssh://), ftp://, ftps://, dav://, davs://, nfs://, afp://."
        ));
    }
    let (authority, path_raw) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err("That address is missing the server — like sftp://server/share.".into());
    }
    // Split userinfo (last '@' wins; an '@' inside a username would be
    // percent-encoded on the wire).
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    let username = match userinfo {
        Some(u) if !u.is_empty() => {
            if u.contains(':') {
                return Err(
                    "Please don't put a password in the address — type it in the password field instead, so it can be kept in the system keyring rather than in the address."
                        .into(),
                );
            }
            Some(u.to_string())
        }
        _ => None,
    };
    // Host + optional port (bracketed IPv6 aware).
    let (host_raw, port) = if let Some(stripped) = hostport.strip_prefix('[') {
        match stripped.find(']') {
            Some(end) => {
                let tail = &stripped[end + 1..];
                let port = if let Some(p) = tail.strip_prefix(':') {
                    Some(parse_port(p)?)
                } else if tail.is_empty() {
                    None
                } else {
                    return Err(format!("That address doesn't parse after the host: “{tail}”."));
                };
                (stripped[..end].to_string(), port)
            }
            None => return Err("That address has an unclosed '[' in the host.".into()),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                (h.to_string(), Some(parse_port(p)?))
            }
            _ => (hostport.to_string(), None),
        }
    };
    let host = host_raw.trim().to_lowercase();
    if host.is_empty() {
        return Err("That address is missing the server — like sftp://server/share.".into());
    }
    let port = match port {
        Some(p) if Some(p) == default_port(&scheme) => None,
        other => other,
    };
    // Path: leading slash guaranteed by the split; drop one trailing
    // slash (but the root stays "/").
    let mut path = path_raw.to_string();
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    let mut uri = format!("{scheme}://");
    if let Some(u) = &username {
        uri.push_str(u);
        uri.push('@');
    }
    if host.contains(':') {
        uri.push('[');
        uri.push_str(&host);
        uri.push(']');
    } else {
        uri.push_str(&host);
    }
    if let Some(p) = port {
        uri.push_str(&format!(":{p}"));
    }
    uri.push_str(&path);
    Ok(ServerUri {
        uri,
        scheme,
        host,
        port,
        username,
        path,
    })
}

fn parse_port(p: &str) -> Result<u16, String> {
    p.parse::<u16>()
        .map_err(|_| format!("“{p}” isn't a usable port number."))
}

/// The sidebar name for a server: the host, plus the final path
/// segment when the address names a share/folder ("nas.local/photos"
/// reads like Finder; two shares on one host stay distinguishable).
pub fn display_name_for(parsed: &ServerUri) -> String {
    if parsed.path == "/" {
        return parsed.host.clone();
    }
    let tail = parsed.path.rsplit('/').next().unwrap_or("");
    if tail.is_empty() {
        parsed.host.clone()
    } else {
        format!("{}/{}", parsed.host, tail)
    }
}

// ---------------------------------------------------------------------------
// Bookmarks — the network-servers.json sidecar (atomic writes, same
// discipline as locations.json / ui-prefs.json).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerBookmark {
    /// Normalized identity URI (never carries a password).
    pub uri: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// True when a password for this server sits in the OS keyring.
    #[serde(default)]
    pub has_saved_password: bool,
}

pub fn bookmarks_path(base: &Path) -> PathBuf {
    base.join("network-servers.json")
}

pub fn load_bookmarks(base: &Path) -> Vec<ServerBookmark> {
    let data = match std::fs::read(bookmarks_path(base)) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    serde_json::from_slice(&data).unwrap_or_default()
}

pub fn save_bookmarks(base: &Path, bookmarks: &[ServerBookmark]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(bookmarks)
        .map_err(|e| format!("bookmark encode: {e}"))?;
    let path = bookmarks_path(base);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("bookmark write: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("bookmark rename: {e}"))?;
    Ok(())
}

pub fn upsert_bookmark(
    base: &Path,
    bookmark: ServerBookmark,
) -> Result<Vec<ServerBookmark>, String> {
    let mut all = load_bookmarks(base);
    match all.iter_mut().find(|b| b.uri == bookmark.uri) {
        Some(existing) => *existing = bookmark,
        None => all.push(bookmark),
    }
    save_bookmarks(base, &all)?;
    Ok(all)
}

pub fn remove_bookmark(base: &Path, uri: &str) -> Result<Vec<ServerBookmark>, String> {
    let mut all = load_bookmarks(base);
    all.retain(|b| b.uri != uri);
    save_bookmarks(base, &all)?;
    Ok(all)
}

// ---------------------------------------------------------------------------
// Keyring — the only place a network password exists.
// ---------------------------------------------------------------------------

fn keyring_entry(uri: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(
        crate::rommedia::KEYRING_SERVICE,
        &format!("network/{uri}"),
    )
    .map_err(|e| format!("system keyring unavailable: {e}"))
}

pub fn saved_password(uri: &str) -> Option<String> {
    let entry = keyring_entry(uri).ok()?;
    match entry.get_password() {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

pub fn store_password(uri: &str, password: &str) -> Result<(), String> {
    keyring_entry(uri)?
        .set_password(password)
        .map_err(|e| format!("could not save the password to the system keyring: {e}"))?;
    // Read back through a fresh Entry before claiming success
    // (2026-10-15 mock-store incident: saves returned Ok into a
    // throwaway in-memory store and evaporated).
    crate::rommedia::verify_readback(&format!("network/{uri}"), password)
}

pub fn forget_password(uri: &str) {
    if let Ok(entry) = keyring_entry(uri) {
        match entry.delete_credential() {
            Ok(()) => {}
            Err(keyring::Error::NoEntry) => {}
            Err(e) => {
                tracing::warn!(target: "ufm", "network keyring delete for {uri}: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// GIO mounts — VolumeMonitor reads; mount/unmount on a dedicated
// thread running its own glib MainContext (the gio async API requires
// the calling thread to own/acquire the thread-default context; the
// Tauri/GTK main loop owns the default context, so we never touch it).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LiveMount {
    /// The GIO root URI of the mount (may carry a trailing slash or a
    /// normalized form of what we asked for — matching is tolerant).
    pub root_uri: String,
    /// The FUSE path the mount is browsable at.
    pub fuse_path: PathBuf,
    pub name: String,
}

/// Every current GIO mount with a local (FUSE) path.
/// Pump a main context briefly so asynchronously-populated state
/// (the union VolumeMonitor's mount list arrives over D-Bus) is
/// actually visible before it is read. Reading the monitor from a
/// thread with no running context returns a stale, usually empty
/// list — the first rig run mounted fine and then "could not find"
/// its own mount for exactly this reason.
fn pump_context(ctx: &glib::MainContext, dur: std::time::Duration) {
    let deadline = std::time::Instant::now() + dur;
    while std::time::Instant::now() < deadline {
        while ctx.iteration(false) {}
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Snapshot the monitor's mounts. MUST be called on the glib thread
/// (see on_glib_thread) with its context already pumped.
fn snapshot_mounts() -> Vec<LiveMount> {
    let monitor = gio::VolumeMonitor::get();
    monitor
        .mounts()
        .iter()
        .filter_map(|m| {
            let root = m.root();
            let fuse_path = root.path()?;
            Some(LiveMount {
                root_uri: root.uri().to_string(),
                fuse_path,
                name: m.name().to_string(),
            })
        })
        .collect()
}

pub fn live_mounts() -> Vec<LiveMount> {
    on_glib_thread(|ctx| {
        pump_context(ctx, std::time::Duration::from_millis(300));
        snapshot_mounts()
    })
    .unwrap_or_default()
}

/// Tolerant URI match for mount lookup: case-insensitive scheme/host,
/// trailing slash ignored, and an exact-userinfo-insensitive compare
/// (GIO may root an sftp mount at the home dir URI, or drop/add the
/// userinfo). A mount also matches when its root is the requested URI
/// with extra path ONLY when the request targeted the server root.
pub(crate) fn uri_matches(root_uri: &str, want: &ServerUri) -> bool {
    let root = match normalize_server_uri(root_uri) {
        Ok(r) => r,
        Err(_) => return false,
    };
    if root.scheme != want.scheme || root.host != want.host || root.port != want.port {
        return false;
    }
    if root.path == want.path {
        return true;
    }
    // A mount rooted SHALLOWER than the request serves it too:
    // gvfsd-sftp roots the mount at the user's home directory even
    // when a subpath was requested (observed live on the proof rig:
    // mount root sftp://user@host/, request …/share). The share
    // lives under the mount root — see effective_mount for the
    // browse-path adjustment. Segment-aware: /share never matches
    // /share-other.
    if root.path == "/"
        || want.path.starts_with(&format!("{}/", root.path.trim_end_matches('/')))
    {
        return true;
    }
    // GIO sometimes roots an sftp mount at the user's home directory
    // when the requested path is the root; the share path is what the
    // FUSE root actually serves, so only root requests adopt it.
    want.path == "/"
}

/// The mount as the REQUESTED location should use it: when GIO
/// rooted the mount shallower than the requested path (sftp home
/// root), the browse root is the mount's FUSE path plus the
/// requested subpath — the share, not the whole home directory.
pub fn effective_mount(m: &LiveMount, want: &ServerUri) -> LiveMount {
    let mut out = m.clone();
    if let Ok(root) = normalize_server_uri(&m.root_uri) {
        if root.path != want.path {
            let rel = if root.path == "/" {
                Some(want.path.trim_start_matches('/'))
            } else if want.path.starts_with(&format!("{}/", root.path)) {
                Some(&want.path[root.path.len() + 1..])
            } else {
                None
            };
            if let Some(rel) = rel {
                if !rel.is_empty() {
                    out.fuse_path = m.fuse_path.join(rel);
                }
            }
        }
    }
    out
}

pub fn find_mount(want: &ServerUri) -> Option<LiveMount> {
    live_mounts()
        .into_iter()
        .find(|m| uri_matches(&m.root_uri, want))
        .map(|m| effective_mount(&m, want))
}

#[derive(Debug, Clone, Default)]
pub struct Credentials {
    pub username: Option<String>,
    pub password: Option<String>,
    /// SMB workgroup/domain.
    pub domain: Option<String>,
    pub anonymous: bool,
}

fn build_mount_operation(creds: &Credentials) -> gio::MountOperation {
    use gio::prelude::*;
    let op = gio::MountOperation::new();
    // Our keyring is the single credential store; never let a backend
    // ALSO stash the password through GIO's own save flow.
    op.set_password_save(gio::PasswordSave::Never);
    let creds = creds.clone();
    op.connect_ask_password(move |op, _message, _default_user, _default_domain, flags| {
        if creds.anonymous && flags.contains(gio::AskPasswordFlags::ANONYMOUS_SUPPORTED) {
            op.set_anonymous(true);
        } else {
            op.set_anonymous(false);
            if let Some(u) = &creds.username {
                op.set_username(Some(u.as_str()));
            }
            if let Some(p) = &creds.password {
                op.set_password(Some(p.as_str()));
            }
            if let Some(d) = &creds.domain {
                op.set_domain(Some(d.as_str()));
            }
        }
        op.reply(gio::MountOperationResult::Handled);
    });
    op
}

/// Render a GIO error in plain language where the common cases have
/// a human meaning; otherwise keep GIO's own words (never swallow).
fn human_gio_error(e: &glib::Error) -> String {
    use gio::IOErrorEnum;
    let msg = e.message().to_string();
    if let Some(io) = e.kind::<IOErrorEnum>() {
        return match io {
            IOErrorEnum::Failed => {
                // The backends put their specifics in the message
                // ("Connection refused", "Login failed"…). Keep it.
                format!("Couldn't connect: {msg}")
            }
            IOErrorEnum::NotSupported => {
                "This system can't connect to that kind of server (the GVfs backend isn't available).".into()
            }
            IOErrorEnum::HostNotFound => {
                "That server couldn't be found — check the address and that you're on the right network.".into()
            }
            IOErrorEnum::ConnectionRefused => {
                "The server refused the connection — it may be off, or not running that service.".into()
            }
            IOErrorEnum::TimedOut => {
                "The server didn't answer in time — it may be asleep, off, or on another network.".into()
            }
            IOErrorEnum::PermissionDenied => {
                "The server refused that sign-in — check the username and password.".into()
            }
            IOErrorEnum::AlreadyMounted => {
                "That server is already connected.".into()
            }
            _ => msg,
        };
    }
    msg
}

/// Run `job` on a fresh thread with its own glib MainContext pushed
/// as thread-default, returning the job's result. The job is expected
/// to run a MainLoop on that context until its async op completes.
fn on_glib_thread<T, F>(job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&glib::MainContext) -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ctx = glib::MainContext::new();
        let out = ctx.with_thread_default(|| job(&ctx));
        let _ = tx.send(out);
    });
    rx.recv()
        .map_err(|_| "the connection thread ended unexpectedly".to_string())?
        .map_err(|e| format!("connection context failed: {e}"))
}

/// Mount `uri` (already normalized) with GIO, answering credential
/// questions from `creds`. Idempotent: an already-mounted server
/// returns its existing mount. Blocks until the mount settles
/// (bounded — a dead server must not hang the app forever).
pub fn mount(uri: &str, creds: &Credentials) -> Result<LiveMount, String> {
    use gio::prelude::*;
    let want = normalize_server_uri(uri)?;
    if let Some(m) = find_mount(&want) {
        return Ok(m);
    }
    let uri_owned = uri.to_string();
    let creds = creds.clone();
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    on_glib_thread(move |ctx| {
        let main_loop = glib::MainLoop::new(Some(ctx), false);
        let file = gio::File::for_uri(&uri_owned);
        let op = build_mount_operation(&creds);
        let loop_quit = main_loop.clone();
        file.mount_enclosing_volume(
            gio::MountMountFlags::NONE,
            Some(&op),
            None::<&gio::Cancellable>,
            move |res| {
                let out = res.map_err(|e| human_gio_error(&e));
                let _ = tx.send(out);
                loop_quit.quit();
            },
        );
        main_loop.run();
    })?;
    match rx.recv_timeout(std::time::Duration::from_secs(120)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(
                "Connecting timed out — the server didn't answer. It may be asleep, off, or on another network."
                    .into(),
            )
        }
    }
    // The mount may take a beat to appear in the monitor's list —
    // and a beat longer for its FUSE root to actually answer (the
    // daemon registers the mount before the root directory stats
    // clean; the proof rig raced exactly this). Only hand back a
    // mount whose root is really there.
    for _ in 0..40 {
        if let Some(m) = find_mount(&want) {
            if m.fuse_path.is_dir() {
                return Ok(m);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    Err("Connected, but that folder could not be found on the server — check the path in the address and try again.".into())
}

/// Unmount the server at `uri` (normalized). Missing mounts are a
/// no-op success: disconnect is a state the user wants, not an op
/// that can half-fail.
pub fn unmount(uri: &str) -> Result<(), String> {
    let want = normalize_server_uri(uri)?;
    if find_mount(&want).is_none() {
        return Ok(());
    }
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    on_glib_thread(move |ctx| {
        // GObjects are thread-affine in gio 0.18 (Mount is !Send) —
        // the lookup happens HERE, on the glib thread; only plain
        // data (the URI record) crosses. Pump first: the monitor's
        // list populates asynchronously over D-Bus.
        pump_context(ctx, std::time::Duration::from_millis(300));
        let monitor = gio::VolumeMonitor::get();
        let target = monitor
            .mounts()
            .into_iter()
            .find(|m| uri_matches(&m.root().uri(), &want));
        let target = match target {
            Some(t) => t,
            None => {
                let _ = tx.send(Ok(()));
                return;
            }
        };
        let main_loop = glib::MainLoop::new(Some(ctx), false);
        let op = gio::MountOperation::new();
        let loop_quit = main_loop.clone();
        target.unmount_with_operation(
            gio::MountUnmountFlags::NONE,
            Some(&op),
            None::<&gio::Cancellable>,
            move |res| {
                let out = res.map_err(|e| human_gio_error(&e));
                let _ = tx.send(out);
                loop_quit.quit();
            },
        );
        main_loop.run();
    })?;
    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("Disconnecting timed out — the server may already be gone.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_schemes_and_case() {
        let s = normalize_server_uri("SFTP://User@NAS.Local/Share/").unwrap();
        assert_eq!(s.uri, "sftp://User@nas.local/Share");
        assert_eq!(s.scheme, "sftp");
        assert_eq!(s.host, "nas.local");
        assert_eq!(s.username.as_deref(), Some("User"));
        assert_eq!(s.path, "/Share");
        // ssh:// is the sftp backend's alias.
        let s = normalize_server_uri("ssh://nas.local").unwrap();
        assert_eq!(s.uri, "sftp://nas.local/");
    }

    #[test]
    fn normalize_drops_default_port_keeps_custom() {
        assert_eq!(
            normalize_server_uri("sftp://h:22/x").unwrap().uri,
            "sftp://h/x"
        );
        assert_eq!(
            normalize_server_uri("sftp://h:2222/x").unwrap().uri,
            "sftp://h:2222/x"
        );
        assert_eq!(
            normalize_server_uri("davs://h:443/x").unwrap().uri,
            "davs://h/x"
        );
    }

    #[test]
    fn normalize_rejects_password_in_url() {
        let err = normalize_server_uri("smb://user:secret@nas/share").unwrap_err();
        assert!(err.contains("password field"), "got: {err}");
    }

    #[test]
    fn normalize_rejects_unknown_scheme_and_garbage() {
        assert!(normalize_server_uri("gopher://h/x").is_err());
        assert!(normalize_server_uri("just-a-host").is_err());
        assert!(normalize_server_uri("smb://").is_err());
    }

    #[test]
    fn display_names_read_like_finder() {
        let s = normalize_server_uri("smb://nas.local/photos").unwrap();
        assert_eq!(display_name_for(&s), "nas.local/photos");
        let s = normalize_server_uri("sftp://nas.local").unwrap();
        assert_eq!(display_name_for(&s), "nas.local");
        let s = normalize_server_uri("dav://127.0.0.1:8971/dav/").unwrap();
        assert_eq!(display_name_for(&s), "127.0.0.1/dav");
    }

    #[test]
    fn bookmarks_roundtrip_and_upsert() {
        let dir = std::env::temp_dir().join(format!(
            "ufm-network-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let b = ServerBookmark {
            uri: "sftp://u@h/x".into(),
            display_name: "h/x".into(),
            username: Some("u".into()),
            has_saved_password: true,
        };
        upsert_bookmark(&dir, b.clone()).unwrap();
        let loaded = load_bookmarks(&dir);
        assert_eq!(loaded, vec![b]);
        // Upsert replaces, never duplicates.
        let mut b2 = loaded[0].clone();
        b2.display_name = "renamed".into();
        upsert_bookmark(&dir, b2).unwrap();
        assert_eq!(load_bookmarks(&dir).len(), 1);
        remove_bookmark(&dir, "sftp://u@h/x").unwrap();
        assert!(load_bookmarks(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Live proof rig — NOT part of the gate suite. Drives this module's
/// own mount path against loopback servers stood up on the box:
///
///   FABLE_NET_SFTP_URI=sftp://fabletest@127.0.0.1/share \
///   FABLE_NET_SFTP_USER=fabletest FABLE_NET_SFTP_PASS=… \
///   FABLE_NET_DAV_URI=dav://127.0.0.1:8971/dav \
///   FABLE_NET_DAV_USER=… FABLE_NET_DAV_PASS=… \
///   cargo test -p fable-files network_live_smoke -- --ignored --nocapture
///
/// Needs the desktop session bus (XDG_RUNTIME_DIR +
/// DBUS_SESSION_BUS_ADDRESS of uid 1000), like every GVfs client.
#[cfg(test)]
mod live_tests {
    use super::*;

    fn roundtrip(tag: &str, uri: &str, user: &str, pass: &str) {
        let want = normalize_server_uri(uri).unwrap();
        let creds = Credentials {
            username: Some(user.to_string()),
            password: Some(pass.to_string()),
            domain: None,
            anonymous: false,
        };
        let m = mount(uri, &creds).unwrap_or_else(|e| {
            eprintln!("{tag}: live mounts at failure: {:?}", live_mounts());
            panic!("{tag}: mount {uri}: {e}")
        });
        println!("{tag}: mounted {uri} at {}", m.fuse_path.display());
        assert!(m.fuse_path.is_dir(), "{tag}: fuse root is a dir");
        // Browse: the fixture tree planted by the rig must be there.
        let names: Vec<String> = std::fs::read_dir(&m.fuse_path)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        println!("{tag}: root listing: {names:?}");
        assert!(names.iter().any(|n| n == "hello.txt"), "{tag}: fixture file visible");
        let hello = std::fs::read_to_string(m.fuse_path.join("hello.txt")).unwrap();
        assert!(hello.contains("fable-network-rig"), "{tag}: fixture contents");
        // Copy IN through the mount (plain fs — the same operations
        // the job engine performs over a path).
        let probe = format!("probe-{tag}.txt");
        std::fs::write(m.fuse_path.join(&probe), b"written through gvfs\n").unwrap();
        let back = std::fs::read(m.fuse_path.join(&probe)).unwrap();
        assert_eq!(back, b"written through gvfs\n");
        std::fs::remove_file(m.fuse_path.join(&probe)).unwrap();
        unmount(uri).unwrap_or_else(|e| panic!("{tag}: unmount {uri}: {e}"));
        // After unmount the record of it must read offline.
        assert!(find_mount(&want).is_none(), "{tag}: mount gone after unmount");
        println!("{tag}: roundtrip OK");
    }

    #[test]
    #[ignore]
    fn network_live_smoke() {
        if let Ok(uri) = std::env::var("FABLE_NET_SFTP_URI") {
            roundtrip(
                "sftp",
                &uri,
                &std::env::var("FABLE_NET_SFTP_USER").unwrap(),
                &std::env::var("FABLE_NET_SFTP_PASS").unwrap(),
            );
        }
        if let Ok(uri) = std::env::var("FABLE_NET_DAV_URI") {
            roundtrip(
                "dav",
                &uri,
                &std::env::var("FABLE_NET_DAV_USER").unwrap_or_default(),
                &std::env::var("FABLE_NET_DAV_PASS").unwrap_or_default(),
            );
        }
    }
}
