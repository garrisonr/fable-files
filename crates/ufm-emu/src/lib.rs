//! ufm-emu: the sandboxed emulation runtime.
//!
//! Spec §8 / §25: the Easter egg — pressing **Space on a ROM file launches it
//! playable inside Quick Look** through an embedded, sandboxed MAME/libretro
//! runtime: keyboard/gamepad mapping, pause/reset, save states, audio,
//! fullscreen. No bundled ROMs, no BIOS — the user supplies their own.
//! GPL compatibility is preserved end to end (hence the workspace license).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ufm_core::{ItemId, Result};

// ---------------------------------------------------------------------------
// Core contract — one trait per emulation backend. MAME today; libretro
// cores plug in behind the same trait tomorrow.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmuSession {
    pub id: String,
    pub rom: ItemId,
    pub platform: String,
    pub state: EmuState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmuState {
    Loading,
    Running,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputMap {
    /// action ("up", "a", "start"…) → input descriptor ("KeyW", "Pad0Btn3"…)
    pub bindings: std::collections::HashMap<String, String>,
}

impl InputMap {
    /// Sane keyboard defaults; gamepad mapping is a UI concern on top.
    pub fn default_keyboard() -> Self {
        let pairs = [
            ("up", "ArrowUp"),
            ("down", "ArrowDown"),
            ("left", "ArrowLeft"),
            ("right", "ArrowRight"),
            ("a", "KeyZ"),
            ("b", "KeyX"),
            ("start", "Enter"),
            ("select", "ShiftRight"),
        ];
        Self {
            bindings: pairs
                .iter()
                .map(|(a, k)| (a.to_string(), k.to_string()))
                .collect(),
        }
    }
}

#[async_trait]
pub trait EmuBackend: Send + Sync {
    fn backend_name(&self) -> &'static str;
    fn supported_platforms(&self) -> Vec<&'static str>;

    async fn launch(&self, rom: &ItemId, platform: &str) -> Result<EmuSession>;
    async fn pause(&self, session: &EmuSession) -> Result<()>;
    async fn resume(&self, session: &EmuSession) -> Result<()>;
    async fn reset(&self, session: &EmuSession) -> Result<()>;

    /// Save states live next to the ROM library, versioned with the core.
    async fn save_state(&self, session: &EmuSession, slot: u8) -> Result<Vec<u8>>;
    async fn load_state(&self, session: &EmuSession, slot: u8, data: &[u8]) -> Result<()>;
}

/// Versioned driver modules keep the binary sane: ship a trimmed driver set,
/// fetch more on demand (spec §25 packaging notes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverModule {
    pub platform: String,
    pub core_version: String,
    pub driver_count: u32,
    pub fetched_on_demand: bool,
}

pub struct MameBackend {
    pub core_version: String,
}

#[async_trait]
impl EmuBackend for MameBackend {
    fn backend_name(&self) -> &'static str {
        "mame"
    }

    fn supported_platforms(&self) -> Vec<&'static str> {
        // Trimmed built-in set; the rest are on-demand driver modules.
        vec!["nes", "snes", "genesis", "arcade"]
    }

    async fn launch(&self, _rom: &ItemId, _platform: &str) -> Result<EmuSession> {
        // TODO(roadmap P1): bind the real MAME embedding (safe FFI boundary,
        // frame delivery into the preview surface, audio routing).
        todo!("mame launch")
    }

    async fn pause(&self, _session: &EmuSession) -> Result<()> {
        todo!("mame pause")
    }

    async fn resume(&self, _session: &EmuSession) -> Result<()> {
        todo!("mame resume")
    }

    async fn reset(&self, _session: &EmuSession) -> Result<()> {
        todo!("mame reset")
    }

    async fn save_state(&self, _session: &EmuSession, _slot: u8) -> Result<Vec<u8>> {
        todo!("mame save_state")
    }

    async fn load_state(&self, _session: &EmuSession, _slot: u8, _data: &[u8]) -> Result<()> {
        todo!("mame load_state")
    }
}
