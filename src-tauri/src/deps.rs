//! deps: the dependency law, as a library (software rule 5 —
//! Dickles, strengthened 2026-10-14): no feature may strand the
//! user on a missing tool. Every external program Fable shells out
//! to is either part of the base system or ensured through here:
//! present → run it; missing → install it through the OS package
//! machinery (PackageKit when the desktop offers it, otherwise
//! the native manager under the polkit prompt) and then run it.
//! The only failure this module returns is a calm, plain-language
//! sentence the caller surfaces where the user already is — and
//! the affordance that produced it stays available as the retry.
//!
//! Callers ensure at the moment of use, inside the command the
//! user just invoked, so the admin prompt always belongs to an
//! action the user understands. Background and opportunistic paths
//! (thumbnailers, duration probers, preview extractors) never
//! call this — a password prompt must never ambush a scroll; the
//! tools they use are ensured at the user-invoked doors instead,
//! and once installed anywhere they light up everywhere.

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Distro families Fable knows package names for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Debian,
    Arch,
    Fedora,
    Suse,
    Unknown,
}

/// Which family does this os-release text describe? ID wins; the
/// ID_LIKE fallback catches derivatives (Zorin→ubuntu→debian,
/// Manjaro→arch, Nobara→fedora).
pub fn family_from_os_release(text: &str) -> Family {
    let mut id = "";
    let mut like = "";
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ID_LIKE=") {
            like = v.trim_matches('"');
        } else if let Some(v) = line.strip_prefix("ID=") {
            id = v.trim_matches('"');
        }
    }
    let hay = format!("{id} {like}").to_lowercase();
    for (needle, fam) in [
        ("debian", Family::Debian),
        ("ubuntu", Family::Debian),
        ("arch", Family::Arch),
        ("fedora", Family::Fedora),
        ("rhel", Family::Fedora),
        ("centos", Family::Fedora),
        ("suse", Family::Suse),
    ] {
        if hay.split_whitespace().any(|t| t == needle) {
            return fam;
        }
    }
    Family::Unknown
}

pub fn distro_family() -> Family {
    std::fs::read_to_string("/etc/os-release")
        .map(|t| family_from_os_release(&t))
        .unwrap_or(Family::Unknown)
}

/// One field out of /etc/os-release (ID, VERSION_ID, …), unquoted.
pub fn os_release_field(key: &str) -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    let prefix = format!("{key}=");
    text.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(|v| v.trim_matches('"').to_string())
    })
}

/// Is `bin` runnable from PATH? (Same test the shell would pass.)
pub fn tool_present(bin: &str) -> bool {
    if bin.contains('/') {
        return Path::new(bin).exists();
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
        })
        .unwrap_or(false)
}

/// Package names per family for a logical tool. `None` = Fable
/// doesn't know how to install this on this family; ensure_tool
/// reports that plainly instead of guessing.
pub fn packages_for(family: Family, logical: &str) -> Option<&'static [&'static str]> {
    Some(match (family, logical) {
        // Music Completion fingerprinting (binary: fpcalc).
        (Family::Debian, "fpcalc") => &["libchromaprint-tools"],
        (Family::Fedora, "fpcalc") => &["chromaprint-tools"],
        (Family::Arch, "fpcalc") => &["chromaprint"],
        (Family::Suse, "fpcalc") => &["chromaprint-tools"],
        // ffmpeg ships ffprobe in the same package everywhere.
        (_, "ffmpeg") => &["ffmpeg"],
        (Family::Debian, "bsdtar") => &["libarchive-tools"],
        (Family::Arch, "bsdtar") => &["libarchive"],
        (Family::Fedora, "bsdtar") => &["bsdtar"],
        (Family::Suse, "bsdtar") => &["bsdtar"],
        // ImageMagick (binary: magick).
        (_, "magick") => &["imagemagick"],
        (_, "xorriso") => &["xorriso"],
        (_, "growisofs") => &["dvd+rw-tools"],
        (Family::Debian, "exiftool") => &["libimage-exiftool-perl"],
        (Family::Arch, "exiftool") => &["perl-image-exiftool"],
        (Family::Fedora, "exiftool") => &["perl-Image-ExifTool"],
        (Family::Suse, "exiftool") => &["exiftool"],
        (_, "parted") => &["parted"],
        (_, "rclone") => &["rclone"],
        (_, "mktorrent") => &["mktorrent"],
        (Family::Debian, "soffice") => &["libreoffice"],
        (Family::Arch, "soffice") => &["libreoffice-fresh"],
        (Family::Fedora, "soffice") => &["libreoffice"],
        (Family::Suse, "soffice") => &["libreoffice"],
        // lp + lpstat (checked via lpstat).
        (Family::Debian, "cups") => &["cups-client"],
        (Family::Arch, "cups") => &["cups"],
        (Family::Fedora, "cups") => &["cups"],
        (Family::Suse, "cups") => &["cups"],
        (Family::Debian, "pipx") => &["pipx"],
        (Family::Arch, "pipx") => &["python-pipx"],
        (Family::Fedora, "pipx") => &["pipx"],
        (Family::Suse, "pipx") => &["pipx"],
        // Poppler tools (checked via pdfinfo; pdftotext/pdftoppm
        // ride in the same package on every family). Gauntlet G1:
        // the PDF doors ensure this instead of degrading silently.
        (Family::Debian, "poppler") => &["poppler-utils"],
        (Family::Arch, "poppler") => &["poppler"],
        (Family::Fedora, "poppler") => &["poppler-utils"],
        (Family::Suse, "poppler") => &["poppler-tools"],
        // Compress → .zip (the `zip` CLI).
        (_, "zip") => &["zip"],
        // RetroArch: the emulation backbone's native fallback
        // channel (Flatpak user install is primary — see emu.rs).
        // Every family ships it under the same name.
        (_, "retroarch") => &["retroarch"],
        // Clipboard helpers, per session type (Wayland / X11).
        (_, "wl-clipboard") => &["wl-clipboard"],
        (_, "xclip") => &["xclip"],
        // GStreamer plugin sets for media playback — the STOCK
        // sets. On Fedora/Suse the full codec sets live in the
        // third-party repos (RPM Fusion / Packman); those ride in
        // through `media_full_packages` after the user's explicit
        // OK at the playback-setup door (Q4 verdict: offer, in
        // plain language, never silently).
        (Family::Debian, "gst-plugins") => &[
            "gstreamer1.0-plugins-base",
            "gstreamer1.0-plugins-good",
            "gstreamer1.0-plugins-bad",
            "gstreamer1.0-plugins-ugly",
            "gstreamer1.0-libav",
        ],
        (Family::Arch, "gst-plugins") => &[
            "gst-plugins-base",
            "gst-plugins-good",
            "gst-plugins-bad",
            "gst-plugins-ugly",
            "gst-libav",
        ],
        (Family::Fedora, "gst-plugins") => &[
            "gstreamer1-plugins-base",
            "gstreamer1-plugins-good",
            "gstreamer1-plugins-bad-free",
            "gstreamer1-plugin-openh264",
        ],
        (Family::Suse, "gst-plugins") => &[
            "gstreamer-plugins-base",
            "gstreamer-plugins-good",
            "gstreamer-plugins-bad",
            "gstreamer-plugins-ugly",
            "gstreamer-plugins-libav",
        ],
        // GVfs core set (daemons + FUSE) for Network Locations.
        // `ensure_gvfs` extends this per scheme on the families
        // that split their backends into separate packages.
        (Family::Debian, "gvfs") => &["gvfs", "gvfs-backends", "gvfs-fuse"],
        (Family::Arch, "gvfs") => &["gvfs"],
        (Family::Fedora, "gvfs") => &["gvfs", "gvfs-fuse"],
        (Family::Suse, "gvfs") => &["gvfs", "gvfs-backends", "gvfs-fuse"],
        _ => return None,
    })
}

/// Run `cmd args…`, capturing output, with a hard timeout so a
/// stuck prompt or a hung mirror can never wedge a command thread
/// forever. Err(()) only on timeout.
fn run_captured(
    cmd: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, ()> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| ())?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().map_err(|_| ());
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(());
                }
                std::thread::sleep(Duration::from_millis(120));
            }
            Err(_) => return Err(()),
        }
    }
}

/// Install packages through the OS machinery: PackageKit's pkcon
/// (the desktop's own software stack) when the system offers it,
/// otherwise the family package manager under pkexec.
pub fn install_packages(packages: &[&str]) -> Result<(), String> {
    if tool_present("pkcon") {
        let mut args: Vec<&str> = vec!["install", "-y", "--noninteractive"];
        args.extend(packages.iter().copied());
        match run_captured("pkcon", &args, Duration::from_secs(900)) {
            Ok(out) if out.status.success() => return Ok(()),
            _ => {
                // Fall through to the native manager before giving
                // up — pkcon resolves names strictly and refuses
                // some valid packages.
            }
        }
    }
    let fam = distro_family();
    let (cmd, base): (&str, &[&str]) = match fam {
        Family::Debian => ("apt-get", &["install", "-y"]),
        Family::Arch => ("pacman", &["-S", "--noconfirm", "--needed"]),
        Family::Fedora => ("dnf", &["install", "-y"]),
        Family::Suse => ("zypper", &["--non-interactive", "install"]),
        Family::Unknown => {
            return Err(
                "Fable doesn't recognize this system's package manager, so it can't run the install itself."
                    .to_string(),
            )
        }
    };
    if fam == Family::Debian {
        // A fresh machine may have empty package lists; refresh
        // them first so the install resolves. Best-effort: if the
        // refresh fails the install usually still can.
        let _ = run_captured(
            "pkexec",
            &["apt-get", "update"],
            Duration::from_secs(300),
        );
    }
    let mut argv: Vec<&str> = vec![cmd];
    argv.extend(base.iter().copied());
    argv.extend(packages.iter().copied());
    match run_captured("pkexec", &argv, Duration::from_secs(900)) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let tail = String::from_utf8_lossy(&out.stderr)
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            if tail.is_empty() {
                Err("the system installer didn't finish the job".to_string())
            } else {
                Err(tail)
            }
        }
        Err(()) => Err(
            "the install didn't complete — the password prompt may have been cancelled, or the installer timed out"
                .to_string(),
        ),
    }
}

/// The law, generalized: make sure a capability is present —
/// `present()` is the caller's own probe (a PATH binary for most
/// tools; daemon files for GVfs; plugin files for GStreamer) —
/// installing the `logical` tool's packages when it isn't. Ok
/// means "run it now". Err is a calm sentence for the surface
/// the user is already looking at; the action that led here is
/// the retry.
pub fn ensure_with_check(
    logical: &str,
    present: &dyn Fn() -> bool,
) -> Result<(), String> {
    if present() {
        return Ok(());
    }
    let fam = distro_family();
    let Some(packages) = packages_for(fam, logical) else {
        return Err(format!(
            "This needs the {logical} tool, and Fable doesn't know the package name for it on this system — so it can't set it up automatically."
        ));
    };
    install_packages(packages).map_err(|e| {
        format!("Fable couldn't set up {logical} on this system: {e}. Nothing else is affected, and you can try again from the same place.")
    })?;
    if present() {
        Ok(())
    } else {
        Err(format!(
            "The {logical} install finished, but the tool still isn't on this system — nothing else is affected, and you can try again from the same place."
        ))
    }
}

/// The law, as one call: make sure `bin` is runnable, installing
/// the `logical` tool's packages when it isn't.
pub fn ensure_tool(bin: &str, logical: &str) -> Result<(), String> {
    ensure_with_check(logical, &|| tool_present(bin))
}

/// Tools whose setup a surface already offered this session and
/// the user let fail (declined prompt, failed install). Preview
/// and reader doors use `ensure_tool_session`: a decline there
/// is respected for the rest of the session instead of becoming
/// a fresh password prompt on every page and preview — the
/// calm receipt is returned again, and explicit install
/// affordances elsewhere keep calling plain `ensure_tool`,
/// which always retries for real.
static SESSION_DECLINED: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

fn session_declined(logical: &str) -> Option<String> {
    let guard = SESSION_DECLINED.lock().unwrap();
    guard.as_ref().and_then(|set| {
        if set.contains(logical) {
            Some(format!(
                "Fable already offered to set up {logical} this session and it didn't go through — nothing else is affected, and restarting Fable (or using a feature that sets it up) lets it try again."
            ))
        } else {
            None
        }
    })
}

pub fn ensure_tool_session(bin: &str, logical: &'static str) -> Result<(), String> {
    if tool_present(bin) {
        return Ok(());
    }
    if let Some(receipt) = session_declined(logical) {
        return Err(receipt);
    }
    match ensure_tool(bin, logical) {
        Ok(()) => Ok(()),
        Err(e) => {
            let mut guard = SESSION_DECLINED.lock().unwrap();
            guard
                .get_or_insert_with(HashSet::new)
                .insert(logical);
            Err(e)
        }
    }
}

// -----------------------------------------------------------------
// GVfs — Network Locations' engine room. The daemons are not
// PATH tools, so presence is probed on the daemon FILES; a
// system that already carries them (any family — including
// ones Fable can't install for) sails through silently.
// -----------------------------------------------------------------

/// Directories the gvfs daemons live in across the families
/// (Debian/Ubuntu populate /usr/lib/gvfs and a /usr/libexec
/// view of the same set; Fedora uses /usr/libexec).
const GVFS_DAEMON_DIRS: [&str; 3] = ["/usr/lib/gvfs", "/usr/libexec", "/usr/lib64/gvfs"];

fn gvfs_daemon_file(name: &str) -> bool {
    GVFS_DAEMON_DIRS.iter().any(|d| Path::new(d).join(name).is_file())
}

/// The daemon binary that serves a Connect scheme. WebDAV's is
/// gvfsd-dav (gvfsd-http is the fallback where DAV is folded
/// into the HTTP backend).
pub fn gvfs_backend_daemon(scheme: &str) -> Option<&'static str> {
    Some(match scheme {
        "smb" => "gvfsd-smb",
        "ftp" | "ftps" => "gvfsd-ftp",
        "sftp" => "gvfsd-sftp",
        "dav" | "davs" => "gvfsd-dav",
        "nfs" => "gvfsd-nfs",
        "afp" => "gvfsd-afp",
        _ => return None,
    })
}

fn gvfs_backend_present(scheme: &str) -> bool {
    match gvfs_backend_daemon(scheme) {
        Some(d) => {
            gvfs_daemon_file(d)
                || (scheme.starts_with("dav") && gvfs_daemon_file("gvfsd-http"))
        }
        None => false,
    }
}

/// Everything the Connect door needs on disk: the core daemon,
/// the FUSE daemon (mounted servers are browsed through it),
/// and the scheme's own backend.
pub fn gvfs_ready(scheme: &str) -> bool {
    gvfs_daemon_file("gvfsd")
        && gvfs_daemon_file("gvfsd-fuse")
        && gvfs_backend_present(scheme)
}

/// The package set that supplies GVfs for `scheme` on
/// `family`: the base `gvfs` row plus the split backend
/// packages on the families that split them (Arch, Fedora).
pub fn gvfs_packages(family: Family, scheme: &str) -> Vec<&'static str> {
    let mut pkgs: Vec<&'static str> = packages_for(family, "gvfs")
        .map(|p| p.to_vec())
        .unwrap_or_default();
    let extra: &[&str] = match (family, scheme) {
        (Family::Arch, "smb") => &["gvfs-smb"],
        (Family::Arch, "nfs") => &["gvfs-nfs"],
        (Family::Arch, "afp") => &["gvfs-afp"],
        (Family::Fedora, "smb") => &["gvfs-smb"],
        (Family::Fedora, "nfs") => &["gvfs-nfs"],
        (Family::Fedora, "afp") => &["gvfs-afp"],
        _ => &[],
    };
    for p in extra {
        if !pkgs.contains(p) {
            pkgs.push(p);
        }
    }
    pkgs
}

/// The Connect door's ensure, run BEFORE any mount attempt:
/// ready → silent Ok; missing → install the family's set and
/// re-check; a backend the family doesn't ship (or an install
/// that didn't go through) earns one plain sentence here, in
/// the dialog the user is already looking at — never a
/// cryptic GIO failure from the mount itself.
pub fn ensure_gvfs(scheme: &str) -> Result<(), String> {
    if gvfs_ready(scheme) {
        return Ok(());
    }
    let fam = distro_family();
    if packages_for(fam, "gvfs").is_none() {
        return Err(
            "This system is missing the file-sharing pieces Fable connects with (GVfs), and Fable doesn't recognize this system's package manager, so it can't set them up automatically."
                .to_string(),
        );
    }
    let pkgs = gvfs_packages(fam, scheme);
    install_packages(&pkgs).map_err(|e| {
        format!("Fable couldn't set up the file-sharing pieces (GVfs) on this system: {e}. Nothing else is affected, and you can try again from the same place.")
    })?;
    if gvfs_ready(scheme) {
        return Ok(());
    }
    let kind = match scheme {
        "smb" => "Windows and Samba shares",
        "ftp" | "ftps" => "FTP servers",
        "sftp" => "SSH and SFTP servers",
        "dav" | "davs" => "WebDAV servers",
        "nfs" => "NFS shares",
        "afp" => "AFP servers (older Macs and Time Capsules)",
        _ => "servers of this kind",
    };
    Err(format!(
        "This system's file-sharing pieces are in place, but they don't include the part that speaks to {kind} — so this connection can't be made from this system. Nothing else is affected."
    ))
}

// -----------------------------------------------------------------
// GStreamer plugin probe. The startup heal (main.rs) restores
// the host's plugin directories to the search path, but it
// cannot heal ABSENCE — a host with no plugin set at all plays
// nothing, not even the transcode fallback's output. Presence
// is probed on the plugin files themselves, in the same
// directories the heal searches.
// -----------------------------------------------------------------

fn gst_plugin_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for var in ["GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_SYSTEM_PATH"] {
        if let Ok(v) = std::env::var(var) {
            for part in v.split(':').filter(|p| !p.is_empty()) {
                let p = std::path::PathBuf::from(part);
                if !dirs.contains(&p) {
                    dirs.push(p);
                }
            }
        }
    }
    for d in [
        "/usr/lib/gstreamer-1.0",
        "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
        "/usr/lib64/gstreamer-1.0",
    ] {
        let p = std::path::PathBuf::from(d);
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }
    dirs
}

fn gst_plugin_file(dirs: &[std::path::PathBuf], name: &str) -> bool {
    dirs.iter().any(|d| d.join(name).is_file())
}

fn gst_plugins_present_in(dirs: &[std::path::PathBuf]) -> bool {
    // playbin (plugins-base) is the player's engine; the libav
    // bridge is the one plugin that decodes every common codec
    // family. Both ride in the mapped set on every family, so
    // "both files exist" is exactly "the set is on this system".
    gst_plugin_file(dirs, "libgstplayback.so")
        && gst_plugin_file(dirs, "libgstlibav.so")
}

/// Is the host's GStreamer playback set on this system?
pub fn gst_plugins_present() -> bool {
    gst_plugins_present_in(&gst_plugin_dirs())
}

// -----------------------------------------------------------------
// Third-party codec sources (Q4 — Dickles' verdict: Fable may
// OFFER to enable them, in plain language, with the user's
// admin OK — never silently). Stock Fedora carries no ffmpeg
// and only the patent-free slice of the codec set (RPM Fusion
// carries the rest); stock openSUSE gates its full codec
// builds the same way (Packman). Enabling is a separate,
// explicit step: the playback-setup door asks first, in
// words, and the pkexec prompt is the user's OK.
// -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThirdPartyRepo {
    pub id: &'static str,
    pub name: &'static str,
}

/// The codec source a family needs beyond its stock repos, if
/// it has one.
pub fn third_party_repo(family: Family) -> Option<ThirdPartyRepo> {
    match family {
        Family::Fedora => Some(ThirdPartyRepo {
            id: "rpmfusion",
            name: "RPM Fusion",
        }),
        Family::Suse => Some(ThirdPartyRepo {
            id: "packman",
            name: "Packman",
        }),
        _ => None,
    }
}

/// Whether the family's codec source is already enabled.
pub fn repo_enabled(family: Family) -> bool {
    match family {
        Family::Fedora => matches!(
            run_captured("rpm", &["-q", "rpmfusion-free-release"], Duration::from_secs(15)),
            Ok(out) if out.status.success()
        ),
        Family::Suse => {
            // zypper keeps one .repo file per repository under
            // /etc/zypp/repos.d; Packman's is named for it.
            let dir = Path::new("/etc/zypp/repos.d");
            match std::fs::read_dir(dir) {
                Ok(entries) => entries.flatten().any(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .to_lowercase()
                        .contains("packman")
                }),
                Err(_) => false,
            }
        }
        _ => false,
    }
}

/// The full media set for the playback-setup door: the stock
/// gst-plugins row plus — on the repo families, and only once
/// the repo is enabled with the user's OK — the pieces only
/// that source carries, and ffmpeg itself, so the conversion
/// doors light up from the same single consent (one promise:
/// media works on this system). On Debian/Arch the stock row
/// already IS the full set.
pub fn media_full_packages(family: Family) -> Vec<&'static str> {
    let mut pkgs: Vec<&'static str> = packages_for(family, "gst-plugins")
        .map(|p| p.to_vec())
        .unwrap_or_default();
    let extra: &[&str] = match family {
        Family::Fedora => &[
            "gstreamer1-plugins-ugly",
            "gstreamer1-plugins-bad-freeworld",
            "gstreamer1-libav",
            "ffmpeg",
        ],
        Family::Suse => &[
            "gstreamer-plugins-bad-codecs",
            "gstreamer-plugins-ugly-codecs",
            "ffmpeg",
        ],
        _ => &[],
    };
    for p in extra {
        if !pkgs.contains(p) {
            pkgs.push(p);
        }
    }
    pkgs
}

/// Enable the family's codec source. Only ever called from
/// the playback-setup door after the user agreed, in words,
/// on that surface — the pkexec prompt is the admin OK.
pub fn enable_third_party_repo(family: Family) -> Result<(), String> {
    let Some(repo) = third_party_repo(family) else {
        return Err("This system has no third-party codec source for Fable to add.".to_string());
    };
    match family {
        Family::Fedora => {
            // The release package registers the repo itself;
            // the version comes from this machine's os-release.
            let ver = os_release_field("VERSION_ID").ok_or_else(|| {
                "Fable couldn't read this system's Fedora version, so it can't pick the right RPM Fusion package.".to_string()
            })?;
            let url = format!(
                "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-{ver}.noarch.rpm"
            );
            match run_captured(
                "pkexec",
                &["dnf", "install", "-y", &url],
                Duration::from_secs(600),
            ) {
                Ok(out) if out.status.success() => Ok(()),
                Ok(out) => {
                    let tail = output_tail(&out);
                    if tail.is_empty() {
                        Err(format!("Adding {} didn't finish.", repo.name))
                    } else {
                        Err(format!("Adding {} didn't finish: {tail}", repo.name))
                    }
                }
                Err(()) => Err(
                    "the password prompt may have been cancelled, or the installer timed out"
                        .to_string(),
                ),
            }
        }
        Family::Suse => {
            // Priority 90 (ahead of the default 99) so Packman's
            // full builds win for the codec packages; then a
            // metadata refresh for just that repo.
            let id = os_release_field("ID").unwrap_or_default();
            let ver = os_release_field("VERSION_ID").unwrap_or_default();
            let branch = if id.contains("tumbleweed") {
                "openSUSE_Tumbleweed".to_string()
            } else {
                format!("openSUSE_Leap_{ver}")
            };
            let url = format!("https://ftp.gwdg.de/pub/linux/misc/packman/suse/{branch}/");
            match run_captured(
                "pkexec",
                &["zypper", "ar", "-cfp", "90", &url, "packman"],
                Duration::from_secs(300),
            ) {
                Ok(out) if out.status.success() => {
                    let _ = run_captured(
                        "pkexec",
                        &[
                            "zypper",
                            "--non-interactive",
                            "--gpg-auto-import-keys",
                            "refresh",
                            "packman",
                        ],
                        Duration::from_secs(300),
                    );
                    Ok(())
                }
                Ok(out) => {
                    let tail = output_tail(&out);
                    if tail.is_empty() {
                        Err(format!("Adding {} didn't finish.", repo.name))
                    } else {
                        Err(format!("Adding {} didn't finish: {tail}", repo.name))
                    }
                }
                Err(()) => Err(
                    "the password prompt may have been cancelled, or the installer timed out"
                        .to_string(),
                ),
            }
        }
        _ => unreachable!("third_party_repo only names Fedora/Suse"),
    }
}

// -----------------------------------------------------------------
// Uninstalling OTHER apps (the Apps surface): the owner query
// and the removal both speak the family's language. This used
// to live inline in the command as pacman-only, which made
// the whole flow error out on every apt/dnf/zypper machine —
// including Dickles' Zorin.
// -----------------------------------------------------------------

/// Parse `dpkg -S <path>`: "pkg[, pkg…]: /path" (a package
/// name may itself carry an ":arch" suffix); diversion lines
/// are not owners.
pub fn parse_dpkg_owner(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("diversion") {
            continue;
        }
        if let Some((owners, _)) = line.rsplit_once(": ") {
            let first = owners.split(',').next()?.trim();
            let pkg = first.split(':').next()?.trim();
            if !pkg.is_empty() {
                return Some(pkg.to_string());
            }
        }
    }
    None
}

/// Parse `pacman -Qo <path>`: "<path> is owned by <pkg> <ver>".
pub fn parse_pacman_owner(text: &str) -> Option<String> {
    text.split(" is owned by ")
        .nth(1)
        .and_then(|t| t.split_whitespace().next())
        .map(|s| s.to_string())
}

/// Parse `rpm -qf <path>`: the bare package name, or nothing
/// when rpm reports the file unowned.
pub fn parse_rpm_owner(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    if line.is_empty() || line.contains("is not owned by any package") {
        None
    } else {
        Some(line.to_string())
    }
}

/// Which package owns `path` on this system? Ok(None) = no
/// package owns it (the caller's "could not find the owning
/// package" case, unchanged). Err = the calm can't-do.
pub fn app_owner_query(path: &str) -> Result<Option<String>, String> {
    let fam = distro_family();
    let (cmd, args): (&str, Vec<&str>) = match fam {
        Family::Debian => ("dpkg", vec!["-S", path]),
        Family::Arch => ("pacman", vec!["-Qo", path]),
        Family::Fedora | Family::Suse => ("rpm", vec!["-qf", path]),
        Family::Unknown => {
            return Err(
                "Fable doesn't recognize this system's package manager, so it can't uninstall system apps here."
                    .to_string(),
            )
        }
    };
    match run_captured(cmd, &args, Duration::from_secs(60)) {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            Ok(match fam {
                Family::Debian => parse_dpkg_owner(&text),
                Family::Arch => parse_pacman_owner(&text),
                _ => parse_rpm_owner(&text),
            })
        }
        Err(()) => Err("the package query didn't answer — it may have timed out".to_string()),
    }
}

/// Remove a system package the family's way, under pkexec.
/// Debian purges — the config goes too, the apt analogue of
/// the `pacman -Rns` the Arch path keeps verbatim.
pub fn remove_app_package(pkg: &str) -> Result<(), String> {
    let fam = distro_family();
    let argv: Vec<&str> = match fam {
        Family::Debian => vec!["apt-get", "purge", "-y", pkg],
        Family::Arch => vec!["pacman", "-Rns", "--noconfirm", pkg],
        Family::Fedora => vec!["dnf", "remove", "-y", pkg],
        Family::Suse => vec!["zypper", "--non-interactive", "remove", pkg],
        Family::Unknown => {
            return Err(
                "Fable doesn't recognize this system's package manager, so it can't uninstall system apps here."
                    .to_string(),
            )
        }
    };
    match run_captured("pkexec", &argv, Duration::from_secs(900)) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let tail = output_tail(&out);
            if tail.is_empty() {
                Err("the uninstall didn't finish the job".to_string())
            } else {
                Err(format!("uninstall failed: {tail}"))
            }
        }
        Err(()) => Err(
            "the uninstall didn't complete — the password prompt may have been cancelled, or the installer timed out"
                .to_string(),
        ),
    }
}

/// Helper for cast setup: the last meaningful line a command
/// printed (used to keep pipx quiet-but-honest in errors).
pub fn output_tail(out: &std::process::Output) -> String {
    let text = if out.stderr.is_empty() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_detection_prefers_id_then_id_like() {
        assert_eq!(
            family_from_os_release("ID=zorin\nID_LIKE=\"ubuntu debian\"\n"),
            Family::Debian
        );
        assert_eq!(family_from_os_release("ID=arch\n"), Family::Arch);
        assert_eq!(
            family_from_os_release("ID=manjaro\nID_LIKE=arch\n"),
            Family::Arch
        );
        assert_eq!(
            family_from_os_release("ID=nobara\nID_LIKE=fedora\n"),
            Family::Fedora
        );
        assert_eq!(
            family_from_os_release("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n"),
            Family::Suse
        );
        assert_eq!(family_from_os_release("ID=void\n"), Family::Unknown);
    }

    #[test]
    fn package_map_covers_the_law_list() {
        for fam in [Family::Debian, Family::Arch, Family::Fedora, Family::Suse] {
            for logical in [
                "fpcalc", "ffmpeg", "bsdtar", "magick", "xorriso", "growisofs",
                "exiftool", "parted", "rclone", "mktorrent", "soffice", "cups",
                "pipx",
                // Gauntlet G1 additions: poppler at the PDF
                // doors, zip at Compress, per-session clipboard
                // helpers, the GStreamer playback set, GVfs.
                "poppler", "zip", "wl-clipboard", "xclip", "gst-plugins",
                "gvfs",
            ] {
                assert!(
                    packages_for(fam, logical).is_some(),
                    "missing mapping: {fam:?}/{logical}"
                );
            }
        }
        assert_eq!(
            packages_for(Family::Debian, "fpcalc"),
            Some(&["libchromaprint-tools"][..])
        );
        assert!(packages_for(Family::Unknown, "ffmpeg").is_some());
        assert!(packages_for(Family::Debian, "nosuchtool").is_none());
    }

    #[test]
    fn g1_package_names_are_the_families_own() {
        // Poppler: the one named law gap (ACCEPTANCE.md) — the
        // binary checked at the doors is pdfinfo, and every
        // family's package also carries pdftotext + pdftoppm.
        assert_eq!(packages_for(Family::Debian, "poppler"), Some(&["poppler-utils"][..]));
        assert_eq!(packages_for(Family::Arch, "poppler"), Some(&["poppler"][..]));
        assert_eq!(packages_for(Family::Fedora, "poppler"), Some(&["poppler-utils"][..]));
        assert_eq!(packages_for(Family::Suse, "poppler"), Some(&["poppler-tools"][..]));
        // The stock Fedora GStreamer set must NOT name RPM
        // Fusion packages — those only arrive via the explicit
        // repo offer (media_full_packages + the user's OK).
        let fedora_stock = packages_for(Family::Fedora, "gst-plugins").unwrap();
        assert!(fedora_stock.contains(&"gstreamer1-plugins-bad-free"));
        assert!(!fedora_stock.contains(&"gstreamer1-plugins-ugly"));
        assert!(!fedora_stock.contains(&"gstreamer1-libav"));
        let full = media_full_packages(Family::Fedora);
        assert!(full.contains(&"gstreamer1-plugins-ugly"));
        assert!(full.contains(&"gstreamer1-libav"));
        assert!(full.contains(&"ffmpeg"));
        // Debian's stock set is already the full set.
        assert_eq!(
            media_full_packages(Family::Debian),
            packages_for(Family::Debian, "gst-plugins").unwrap().to_vec()
        );
        // GVfs: base set everywhere; split backends ride along
        // per scheme only on the families that split them.
        assert_eq!(
            gvfs_packages(Family::Debian, "smb"),
            vec!["gvfs", "gvfs-backends", "gvfs-fuse"]
        );
        assert_eq!(
            gvfs_packages(Family::Arch, "smb"),
            vec!["gvfs", "gvfs-smb"]
        );
        assert_eq!(gvfs_packages(Family::Arch, "sftp"), vec!["gvfs"]);
        assert_eq!(
            gvfs_packages(Family::Fedora, "nfs"),
            vec!["gvfs", "gvfs-fuse", "gvfs-nfs"]
        );
    }

    #[test]
    fn family_detection_covers_the_gauntlet_twenty() {
        // Real /etc/os-release shapes for the 20 gauntlet
        // distros (AUDIT §3). ID wins; ID_LIKE catches the
        // derivatives.
        let cases: &[(&str, Family)] = &[
            ("ID=cachyos\nID_LIKE=arch\n", Family::Arch),
            ("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n", Family::Debian),
            ("ID=mx\nID_LIKE=debian\n", Family::Debian),
            ("ID=endeavouros\nID_LIKE=arch\n", Family::Arch),
            ("ID=debian\n", Family::Debian),
            ("ID=ubuntu\nID_LIKE=debian\n", Family::Debian),
            ("ID=fedora\n", Family::Fedora),
            ("ID=zorin\nID_LIKE=\"ubuntu debian\"\n", Family::Debian),
            ("ID=manjaro\nID_LIKE=arch\n", Family::Arch),
            ("ID=pop\nID_LIKE=\"ubuntu debian\"\n", Family::Debian),
            ("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n", Family::Suse),
            ("ID=garuda\nID_LIKE=arch\n", Family::Arch),
            ("ID=elementary\nID_LIKE=\"ubuntu debian\"\n", Family::Debian),
            ("ID=ubuntu\n", Family::Debian), // Kubuntu reports ID=ubuntu
            ("ID=arch\n", Family::Arch),
            ("ID=nixos\nID_LIKE=\n", Family::Unknown),
            ("ID=neon\nID_LIKE=\"ubuntu debian\"\n", Family::Debian),
            ("ID=nobara\nID_LIKE=fedora\n", Family::Fedora),
            ("ID=linuxlite\nID_LIKE=ubuntu\n", Family::Debian),
            ("ID=arcolinux\nID_LIKE=arch\n", Family::Arch),
        ];
        for (text, want) in cases {
            assert_eq!(family_from_os_release(text), *want, "os-release: {text:?}");
        }
    }

    #[test]
    fn gvfs_scheme_daemon_map() {
        assert_eq!(gvfs_backend_daemon("smb"), Some("gvfsd-smb"));
        assert_eq!(gvfs_backend_daemon("ftp"), Some("gvfsd-ftp"));
        assert_eq!(gvfs_backend_daemon("ftps"), Some("gvfsd-ftp"));
        assert_eq!(gvfs_backend_daemon("sftp"), Some("gvfsd-sftp"));
        assert_eq!(gvfs_backend_daemon("dav"), Some("gvfsd-dav"));
        assert_eq!(gvfs_backend_daemon("davs"), Some("gvfsd-dav"));
        assert_eq!(gvfs_backend_daemon("nfs"), Some("gvfsd-nfs"));
        assert_eq!(gvfs_backend_daemon("afp"), Some("gvfsd-afp"));
        assert_eq!(gvfs_backend_daemon("gopher"), None);
    }

    #[test]
    fn gst_probe_needs_playbin_and_libav() {
        let base = std::env::temp_dir().join(format!(
            "fable-gst-probe-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let dirs = vec![base.clone()];
        assert!(!gst_plugins_present_in(&dirs));
        std::fs::write(base.join("libgstplayback.so"), b"").unwrap();
        assert!(!gst_plugins_present_in(&dirs));
        std::fs::write(base.join("libgstlibav.so"), b"").unwrap();
        assert!(gst_plugins_present_in(&dirs));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn codec_repo_table() {
        assert_eq!(third_party_repo(Family::Fedora).unwrap().name, "RPM Fusion");
        assert_eq!(third_party_repo(Family::Suse).unwrap().name, "Packman");
        assert!(third_party_repo(Family::Debian).is_none());
        assert!(third_party_repo(Family::Arch).is_none());
        assert!(third_party_repo(Family::Unknown).is_none());
        // Debian never claims a codec repo is "enabled".
        assert!(!repo_enabled(Family::Debian));
        assert!(!repo_enabled(Family::Unknown));
    }

    #[test]
    fn uninstaller_owner_parsers() {
        assert_eq!(
            parse_dpkg_owner("libreoffice-core: /usr/share/applications/libreoffice-writer.desktop\n"),
            Some("libreoffice-core".to_string())
        );
        assert_eq!(
            parse_dpkg_owner("libc6:amd64: /usr/lib/x86_64-linux-gnu/libc.so.6\n"),
            Some("libc6".to_string())
        );
        assert_eq!(
            parse_dpkg_owner("diversion by dash from: /bin/sh\n"),
            None
        );
        assert_eq!(
            parse_pacman_owner("/usr/share/applications/firefox.desktop is owned by firefox 128.0-1\n"),
            Some("firefox".to_string())
        );
        assert_eq!(
            parse_rpm_owner("firefox-128.0-1.fc41.x86_64\n"),
            Some("firefox-128.0-1.fc41.x86_64".to_string())
        );
        assert_eq!(
            parse_rpm_owner("file /usr/share/applications/x.desktop is not owned by any package"),
            None
        );
        assert_eq!(parse_rpm_owner(""), None);
    }

    #[test]
    fn tool_present_finds_coreutils_not_ghosts() {
        assert!(tool_present("ls"));
        assert!(!tool_present("fable-no-such-binary-xyz"));
        assert!(!tool_present("/nonexistent/fable-no-such-binary"));
    }
}
