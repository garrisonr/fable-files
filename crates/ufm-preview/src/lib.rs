//! ufm-preview: the Quick Look service.
//!
//! Spec §8: pressing Space shows an instant preview — images, audio/video
//! playback, documents, ebooks, fonts, disk images, 3D/CAD, retro formats,
//! archives browsed like folders. Unknown types fall back to metadata / hex /
//! text extraction. Providers are pluggable, sandboxed, cancelable, and
//! budgeted; previews never auto-run content.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ufm_core::Result;
use ufm_vfs::Item;

// ---------------------------------------------------------------------------
// Preview kinds, in preference order. The registry picks the richest kind
// a provider can serve for the item.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    Native,   // the shell can render it directly (image, text, pdf…)
    Media,    // audio/video playback surface
    Document, // paginated document / ebook
    Archive,  // browsed like a folder (zip, 7z, rar, tar…)
    Metadata, // extracted metadata + thumbnail when content can't render
    Hex,      // fallback: hex + text extraction
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewRequest {
    pub item: Item,
    /// Max bytes the provider may read for this preview (budget).
    pub byte_budget: u64,
    /// Cancellation: dropping the request's token aborts decode work.
    pub priority: PreviewPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewPriority {
    Foreground, // the item under the cursor — decode now
    Thumbnail,  // grid thumbnails — decode when idle
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub kind: PreviewKind,
    /// Opaque payload the shell knows how to mount per kind
    /// (stream handle id, page count, archive listing…).
    pub payload_json: String,
    pub title: String,
}

// ---------------------------------------------------------------------------
// Provider contract. Decoders run sandboxed; a provider declares the kinds
// it can serve and the extensions it claims.
// ---------------------------------------------------------------------------

#[async_trait]
pub trait PreviewProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn claims(&self, item: &Item) -> Option<PreviewKind>;
    async fn render(&self, req: &PreviewRequest) -> Result<Preview>;
}

pub struct PreviewRegistry {
    providers: Vec<Box<dyn PreviewProvider>>,
}

impl PreviewRegistry {
    pub fn new() -> Self {
        Self { providers: Vec::new() }
    }

    pub fn register(&mut self, provider: Box<dyn PreviewProvider>) {
        self.providers.push(provider);
    }

    /// Richest available kind wins. Returns None when nothing claims the item
    /// (the shell then shows the Hex fallback itself).
    pub fn best_kind(&self, item: &Item) -> Option<PreviewKind> {
        self.providers
            .iter()
            .filter_map(|p| p.claims(item))
            .max()
    }

    pub fn provider_for(&self, kind: PreviewKind, item: &Item) -> Option<&dyn PreviewProvider> {
        self.providers
            .iter()
            .find(|p| p.claims(item) == Some(kind))
            .map(|p| p.as_ref())
    }

    pub fn provider_names(&self) -> HashMap<&'static str, usize> {
        let mut map = HashMap::new();
        for p in &self.providers {
            *map.entry(p.name()).or_insert(0) += 1;
        }
        map
    }
}

impl Default for PreviewRegistry {
    fn default() -> Self {
        Self::new()
    }
}
