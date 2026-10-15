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

use std::path::Path;
use std::process::{Command, Stdio};
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
fn install_packages(packages: &[&str]) -> Result<(), String> {
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

/// The law, as one call: make sure `bin` is runnable, installing
/// the `logical` tool's packages when it isn't. Ok means "run it
/// now". Err is a calm sentence for the surface the user is
/// already looking at; the action that led here is the retry.
pub fn ensure_tool(bin: &str, logical: &str) -> Result<(), String> {
    if tool_present(bin) {
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
    if tool_present(bin) {
        Ok(())
    } else {
        Err(format!(
            "The {logical} install finished, but the tool still isn't on this system — nothing else is affected, and you can try again from the same place."
        ))
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
    fn tool_present_finds_coreutils_not_ghosts() {
        assert!(tool_present("ls"));
        assert!(!tool_present("fable-no-such-binary-xyz"));
        assert!(!tool_present("/nonexistent/fable-no-such-binary"));
    }
}
