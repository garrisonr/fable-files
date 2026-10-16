//! ufm-emu: the emulator resolution layer (SPEC-AMENDMENT "Emulation",
//! Architecture A — Fable-as-surface).
//!
//! This crate is the pure, testable half of Play:
//!
//! - [`es`] mines ES-DE's bundled `es_systems.xml` (vendored snapshot
//!   in `data/`, ES-DE is MIT-licensed; see the About attribution) for
//!   system definitions and launch templates in ES-DE's own
//!   preference order.
//! - [`table`] turns the mined data into Fable's systems table and
//!   applies the curated override layer (defaults, verdict lines,
//!   install channels, honesty states) from the amendment §3.
//! - [`resolve`] classifies each option against a probe of the actual
//!   machine and makes the ask-once sheet decision (§6): play now, or
//!   show the sheet with this pre-selection.
//! - [`expand`] expands ES-DE launch templates (`%ROM%`, `%GAMEDIR%`,
//!   `%EMULATOR_*%`, `%CORE_RETROARCH%`, …) into an argv. Fable always
//!   launches the game's REAL path from the catalog — never a
//!   unified-view path (the amendment's prime directive).
//! - [`farm`] computes the unified view's desired symlink set and the
//!   delta against reality (create / repoint / retire — never a
//!   wholesale rebuild, never a dangling link).
//! - [`playlist`] writes RetroArch `.lpl` playlists (absolute real
//!   paths — for RetroArch, the playlist IS the unified view).
//! - [`zipx`] extracts one entry from a zip in memory (libretro
//!   buildbot cores ship as single-file zips).
//!
//! Anything that touches the machine (probing installs, downloading,
//! spawning, writing links) lives in the app crate's `emu` module and
//! feeds this crate plain data.

pub mod es;
pub mod expand;
pub mod farm;
pub mod playlist;
pub mod resolve;
pub mod table;
pub mod zipx;

pub use expand::{expand_template, Expanded};
pub use farm::{desired_links, farm_delta, DesiredLink, FarmCopy, FarmGame, FarmOp};
pub use playlist::{playlist_json, PlaylistEntry};
pub use resolve::{decide, option_state, Decision, EnvProbe, OptState, RaFlavor};
pub use table::{systems_table, EmuOption, InstallSpec, OptClass, OptionKind, SystemEntry};
