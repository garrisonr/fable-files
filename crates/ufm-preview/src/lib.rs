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
use std::path::Path;
use ufm_core::Result;
use ufm_vfs::{Item, ItemKind};

pub mod epub;
pub mod pdf;

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

// ---------------------------------------------------------------------------
// ExtensionProvider — the built-in claim router. Maps file extensions (and
// folders) to the richest PreviewKind the shell can serve for them; real
// decoders replace the stub render() in later milestones.
// ---------------------------------------------------------------------------

/// Built-in provider: claims items purely by extension. Folders always map
/// to [`PreviewKind::Metadata`]; anything unrecognized falls back to
/// [`PreviewKind::Hex`] so the shell still shows *something*.
pub struct ExtensionProvider;

impl ExtensionProvider {
    fn extension(item: &Item) -> Option<String> {
        Path::new(&item.name)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
    }
}

#[async_trait]
impl PreviewProvider for ExtensionProvider {
    fn name(&self) -> &'static str {
        "extension"
    }

    fn claims(&self, item: &Item) -> Option<PreviewKind> {
        if item.kind == ItemKind::Folder {
            return Some(PreviewKind::Metadata);
        }
        let ext = Self::extension(item);
        Some(match ext.as_deref() {
            // Native: images and plain text the shell renders directly.
            Some(
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "svg"
                | "ico" | "txt" | "md" | "markdown" | "rs" | "ts" | "js" | "json" | "toml"
                | "yaml" | "yml" | "xml" | "html" | "htm" | "css" | "srt" | "vtt"
                | "sub" | "ssa" | "ass" | "log" | "csv" | "tsv" | "nfo",
            ) => PreviewKind::Native,
            // Media: audio/video playback surface.
            Some(
                "mp3" | "flac" | "ogg" | "oga" | "wav" | "m4a" | "m4b" | "opus" | "mp4"
                | "mkv" | "webm" | "mov" | "avi",
            ) => PreviewKind::Media,
            // Document: paginated document / ebook. The ODF family
            // (odt/ods/odp) shares the content.xml text mechanism.
            Some("pdf" | "epub" | "doc" | "docx" | "odt" | "ods" | "odp") => PreviewKind::Document,
            // Archive: browsed like a folder.
            Some("zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz") => PreviewKind::Archive,
            // Everything else — unknown or missing extension: hex fallback.
            _ => PreviewKind::Hex,
        })
    }

    async fn render(&self, req: &PreviewRequest) -> Result<Preview> {
        // MVP stub: real decoders arrive per-kind in later milestones.
        Ok(Preview {
            kind: self.claims(&req.item).unwrap_or(PreviewKind::Hex),
            payload_json: "{}".to_string(),
            title: req.item.name.clone(),
        })
    }
}

/// Document provider (Phase 4): real PDF/EPUB decoders — see the
/// [`pdf`] and [`epub`] modules. Claims the two formats the decoders
/// serve and renders a structured payload (book/document metadata,
/// page or chapter count, excerpt, plain-language note). Image bytes
/// (rendered page, book cover) deliberately stay out of the payload:
/// the shell fetches them through its cached document-preview path.
pub struct DocumentProvider;

/// Width the PDF first-page render targets when a consumer renders
/// through this provider (the shell's Quick Look uses the same cap).
pub const DOCUMENT_PREVIEW_WIDTH_PX: u32 = 1000;

impl DocumentProvider {
    fn extension(item: &Item) -> Option<String> {
        Path::new(&item.name)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
    }
}

#[async_trait]
impl PreviewProvider for DocumentProvider {
    fn name(&self) -> &'static str {
        "document"
    }

    fn claims(&self, item: &Item) -> Option<PreviewKind> {
        if item.kind == ItemKind::Folder {
            return None;
        }
        match Self::extension(item).as_deref() {
            Some("pdf" | "epub") => Some(PreviewKind::Document),
            _ => None,
        }
    }

    async fn render(&self, req: &PreviewRequest) -> Result<Preview> {
        let ext = Self::extension(&req.item).unwrap_or_default();
        let path = req
            .item
            .provider_key
            .clone()
            .unwrap_or_else(|| req.item.name.clone());
        let payload = match ext.as_str() {
            "pdf" => {
                let size = req.item.size_bytes.unwrap_or(0);
                let p = pdf::decode_pdf(Path::new(&path), size, DOCUMENT_PREVIEW_WIDTH_PX);
                serde_json::json!({
                    "format": "pdf",
                    "title": p.title,
                    "author": p.author,
                    "pages": p.page_count,
                    "has_page_image": p.page_png.is_some(),
                    "encrypted": p.encrypted,
                    "excerpt": p.text,
                    "note": p.note,
                })
            }
            "epub" => {
                let bytes = std::fs::read(&path).unwrap_or_default();
                let budget = req.byte_budget as usize;
                let p = if bytes.len() > budget && budget > 0 {
                    let mut p = epub::EpubPreview::default();
                    p.note = Some(
                        "This book is too large for a preview decode.".into(),
                    );
                    p
                } else {
                    epub::decode_epub(&bytes)
                };
                serde_json::json!({
                    "format": "epub",
                    "title": p.title,
                    "author": p.author,
                    "chapters": p.spine_count,
                    "has_cover": p.cover.is_some(),
                    "cover_media_type": p.cover.as_ref().map(|c| c.media_type.clone()),
                    "drm_protected": p.drm_protected,
                    "excerpt": p.excerpt,
                    "note": p.note,
                })
            }
            _ => serde_json::json!({}),
        };
        Ok(Preview {
            kind: PreviewKind::Document,
            payload_json: payload.to_string(),
            title: req.item.name.clone(),
        })
    }
}

/// Registry pre-loaded with the built-in providers. The shell (and Tauri
/// commands) should start from this, then register plugin providers on top.
pub fn registry_with_builtins() -> PreviewRegistry {
    let mut registry = PreviewRegistry::new();
    registry.register(Box::new(ExtensionProvider));
    registry.register(Box::new(DocumentProvider));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use ufm_core::{ItemId, LocationId};

    fn item(name: &str, kind: ItemKind) -> Item {
        Item {
            id: ItemId::new(),
            location: LocationId::new(),
            name: name.into(),
            kind,
            size_bytes: None,
            modified: None,
            provider_key: None,
            labels: vec![],
            starred: false,
        }
    }

    #[test]
    fn extension_claims_map_to_expected_kinds() {
        let p = ExtensionProvider;
        let cases = [
            ("photo.png", PreviewKind::Native),
            ("photo.JPG", PreviewKind::Native), // case-insensitive
            ("notes.txt", PreviewKind::Native),
            ("lib.rs", PreviewKind::Native),
            ("page.html", PreviewKind::Native),
            ("subs.srt", PreviewKind::Native),
            ("subs.SRT", PreviewKind::Native), // case-insensitive
            ("debug.log", PreviewKind::Native),
            ("data.csv", PreviewKind::Native),
            ("song.flac", PreviewKind::Media),
            ("song.MP3", PreviewKind::Media),
            ("audiobook.m4b", PreviewKind::Media),
            ("audiobook.M4B", PreviewKind::Media), // case-insensitive
            ("movie.mkv", PreviewKind::Media),
            ("clip.mov", PreviewKind::Media),
            ("book.pdf", PreviewKind::Document),
            ("book.epub", PreviewKind::Document),
            ("report.docx", PreviewKind::Document),
            ("letter.odt", PreviewKind::Document),
            ("sheet.ods", PreviewKind::Document),
            ("slides.odp", PreviewKind::Document),
            ("sheet.ODS", PreviewKind::Document), // case-insensitive
            ("archive.zip", PreviewKind::Archive),
            ("backup.tar", PreviewKind::Archive),
            ("data.7z", PreviewKind::Archive),
            ("weird.xyz", PreviewKind::Hex),
            ("noextension", PreviewKind::Hex),
        ];
        for (name, want) in cases {
            assert_eq!(
                p.claims(&item(name, ItemKind::File)),
                Some(want),
                "wrong claim for {name}"
            );
        }
        // Folders always get Metadata, regardless of name.
        assert_eq!(
            p.claims(&item("some.zip", ItemKind::Folder)),
            Some(PreviewKind::Metadata)
        );
    }

    #[tokio::test]
    async fn render_returns_minimal_preview() {
        let p = ExtensionProvider;
        let req = PreviewRequest {
            item: item("report.pdf", ItemKind::File),
            byte_budget: 1024 * 1024,
            priority: PreviewPriority::Foreground,
        };
        let preview = p.render(&req).await.unwrap();
        assert_eq!(preview.kind, PreviewKind::Document);
        assert_eq!(preview.title, "report.pdf");
        assert_eq!(preview.payload_json, "{}");
        // Unknown type still renders — Hex fallback, never an error.
        let req = PreviewRequest {
            item: item("blob.xyz", ItemKind::File),
            byte_budget: 64,
            priority: PreviewPriority::Thumbnail,
        };
        let preview = p.render(&req).await.unwrap();
        assert_eq!(preview.kind, PreviewKind::Hex);
    }

    #[test]
    fn registry_with_builtins_serves_all_kinds() {
        let registry = registry_with_builtins();
        assert_eq!(registry.best_kind(&item("a.png", ItemKind::File)), Some(PreviewKind::Native));
        assert_eq!(registry.best_kind(&item("a.mp3", ItemKind::File)), Some(PreviewKind::Media));
        assert_eq!(
            registry.best_kind(&item("a.pdf", ItemKind::File)),
            Some(PreviewKind::Document)
        );
        assert_eq!(
            registry.best_kind(&item("a.zip", ItemKind::File)),
            Some(PreviewKind::Archive)
        );
        assert_eq!(
            registry.best_kind(&item("dir", ItemKind::Folder)),
            Some(PreviewKind::Metadata)
        );
        assert_eq!(registry.best_kind(&item("a.xyz", ItemKind::File)), Some(PreviewKind::Hex));
        assert!(registry.provider_names().contains_key("extension"));
    }

    #[test]
    fn document_provider_claims_only_pdf_and_epub() {
        let p = DocumentProvider;
        assert_eq!(p.claims(&item("book.pdf", ItemKind::File)), Some(PreviewKind::Document));
        assert_eq!(p.claims(&item("book.EPUB", ItemKind::File)), Some(PreviewKind::Document));
        assert_eq!(p.claims(&item("report.docx", ItemKind::File)), None);
        assert_eq!(p.claims(&item("book.pdf", ItemKind::Folder)), None);
        let registry = registry_with_builtins();
        assert!(registry.provider_names().contains_key("document"));
    }

    #[tokio::test]
    async fn document_render_of_missing_file_degrades_to_note() {
        let p = DocumentProvider;
        let req = PreviewRequest {
            item: item("ghost.epub", ItemKind::File),
            byte_budget: 1024 * 1024,
            priority: PreviewPriority::Foreground,
        };
        let preview = p.render(&req).await.unwrap();
        assert_eq!(preview.kind, PreviewKind::Document);
        let payload: serde_json::Value =
            serde_json::from_str(&preview.payload_json).unwrap();
        assert_eq!(payload["format"], "epub");
        assert!(payload["note"].is_string(), "payload: {payload}");
    }
}
