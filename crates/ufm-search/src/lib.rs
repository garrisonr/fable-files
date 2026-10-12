//! ufm-search: the query language and index feeds.
//!
//! Spec §13: X1-class background indexing. One search language everywhere —
//! keywords, labels, types, dates, sizes, Boolean operators, inside-file
//! content — with results that update as the query is typed. Saved searches
//! power smart folders. Sub-100ms feel on large libraries comes from the
//! catalog's FTS tables, never from live tree-walking.

use serde::{Deserialize, Serialize};
use ufm_core::{CollectionId, LocationId, Result, UfmError};

pub mod indexer;
pub use indexer::{Indexer, IndexStats};

// ---------------------------------------------------------------------------
// Query AST — parsed once, executed against the catalog index.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Query {
    /// Plain keywords: `super mario`
    Text(String),
    /// `type:pdf`, `label:taxes`, `size:>10mb`, `modified:this-week` …
    Facet { field: FacetField, op: FacetOp, value: String },
    And(Vec<Query>),
    Or(Vec<Query>),
    Not(Box<Query>),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacetField {
    Name,
    Type,
    Label,
    Size,
    Modified,
    Location,
    MediaTitle,
    MediaPerson,
    MediaPlatform,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacetOp {
    Eq,
    Contains,
    Gt,
    Lt,
    Before,
    After,
}

/// Parse the search box. Tiny hand-rolled parser for v0.1 — facet syntax is
/// `field:value`, quotes group phrases, AND/OR/NOT are recognized.
/// A real grammar (pest/nom) replaces this before 1.0.
pub fn parse(input: &str) -> Result<Query> {
    let input = input.trim();
    if input.is_empty() {
        return Err(UfmError::InvalidInput("empty query".into()));
    }
    // v0.1: split on top-level OR, then AND, then facet/text atoms.
    let mut or_parts = Vec::new();
    for or_branch in split_top_level(input, " OR ") {
        let mut and_parts = Vec::new();
        for atom in split_top_level(or_branch, " AND ") {
            let atom = atom.trim();
            if let Some(rest) = atom.strip_prefix("NOT ") {
                and_parts.push(Query::Not(Box::new(parse_atom(rest.trim())?)));
            } else {
                and_parts.push(parse_atom(atom)?);
            }
        }
        or_parts.push(if and_parts.len() == 1 {
            and_parts.into_iter().next().unwrap()
        } else {
            Query::And(and_parts)
        });
    }
    Ok(if or_parts.len() == 1 {
        or_parts.into_iter().next().unwrap()
    } else {
        Query::Or(or_parts)
    })
}

fn parse_atom(atom: &str) -> Result<Query> {
    if let Some((field, value)) = atom.split_once(':') {
        let field = match field.to_lowercase().as_str() {
            "type" => FacetField::Type,
            "label" => FacetField::Label,
            "size" => FacetField::Size,
            "modified" => FacetField::Modified,
            "title" => FacetField::MediaTitle,
            "person" => FacetField::MediaPerson,
            "platform" => FacetField::MediaPlatform,
            _ => return Ok(Query::Text(atom.to_string())),
        };
        return Ok(Query::Facet {
            field,
            op: FacetOp::Contains,
            value: value.trim_matches('"').to_string(),
        });
    }
    Ok(Query::Text(atom.trim_matches('"').to_string()))
}

fn split_top_level<'a>(input: &'a str, sep: &str) -> Vec<&'a str> {
    // v0.1: no quote-awareness for the separator itself; good enough for atoms.
    input.split(sep).collect()
}

// ---------------------------------------------------------------------------
// FTS5 rendering — turns a Query AST into an `items_fts MATCH` expression
// for `Catalog::search_fts`.
// ---------------------------------------------------------------------------

/// Render `q` as an FTS5 `MATCH` expression over the catalog's `items_fts`
/// table (name index).
///
/// Mapping (MVP, deliberately simple — the FTS table only indexes names):
/// - `Text(s)` → each word becomes a prefix term `word*`; words are joined
///   with a space, which FTS5 treats as implicit AND.
/// - `Facet { .. }` → treated as plain text: the facet *value* becomes
///   prefix terms (e.g. `type:pdf` → `pdf*`, which matches `report.pdf`
///   because the FTS tokenizer splits on `.`). Facet-aware filtering
///   (size ranges, dates, labels…) is a later milestone.
/// - `And(parts)` → non-empty parts joined with a space (implicit AND).
///   `Not` parts are rendered after positive parts so the result is always
///   of the form `a* b* NOT (c*)`, which FTS5 accepts.
/// - `Or(parts)` → non-empty parts joined with ` OR `, wrapped in
///   parentheses: `(a* OR b*)`.
/// - `Not(q)` → `NOT (<inner>)`. NOTE: FTS5 has no unary `-term` operator
///   (a leading `-` is parsed as a column filter and fails), so the binary
///   `NOT` form is used instead. A *bare* `Not` (`NOT (x*)` alone) is not
///   valid FTS5 — always combine it with a positive term via `And`.
///
/// Words are split on whitespace and on every FTS5 special character
/// (`" * ( ) : { } [ ] ^ + -` and friends); empty fragments are dropped, so
/// the result is always a syntactically safe MATCH expression — or the
/// empty string when nothing renderable remains (which `search_fts` treats
/// as "no rows").
pub fn to_fts(q: &Query) -> String {
    match q {
        Query::Text(s) => words(s).join(" "),
        // MVP: facets are plain text over the indexed name.
        Query::Facet { value, .. } => words(value).join(" "),
        Query::And(parts) => {
            // Positive parts first, negations last: FTS5's NOT is binary and
            // needs a left-hand operand.
            let (negs, poss): (Vec<&Query>, Vec<&Query>) =
                parts.iter().partition(|p| matches!(p, Query::Not(_)));
            let mut out: Vec<String> = poss.iter().map(|p| to_fts(p)).filter(|s| !s.is_empty()).collect();
            out.extend(negs.iter().map(|p| to_fts(p)).filter(|s| !s.is_empty()));
            out.join(" ")
        }
        Query::Or(parts) => {
            let inner: Vec<String> = parts
                .iter()
                .map(to_fts)
                .filter(|s| !s.is_empty())
                .collect();
            match inner.len() {
                0 => String::new(),
                1 => inner.into_iter().next().unwrap(),
                _ => format!("({})", inner.join(" OR ")),
            }
        }
        Query::Not(inner) => {
            let s = to_fts(inner);
            if s.is_empty() {
                String::new()
            } else {
                format!("NOT ({s})")
            }
        }
    }
}

/// Split raw text into FTS5-safe prefix terms: fragments on whitespace and
/// FTS5 special characters, dropped when empty, `*` appended for prefix
/// matching.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '*' | '(' | ')' | ':' | '{' | '}' | '[' | ']' | '^' | '+'
                    | '-' | ',' | '.' | ';' | '!' | '?' | '/' | '\\' | '|'
                    | '&' | '=' | '<' | '>' | '~' | '@' | '#' | '$' | '%'
                    | '\'' | '`'
            )
    })
    .filter(|w| !w.is_empty())
    .map(|w| format!("{w}*"))
    .collect()
}

// ---------------------------------------------------------------------------
// Search request / execution surface (the catalog feeds this).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: Query,
    /// None = global. Some = scoped to one location.
    pub scope: Option<LocationId>,
    pub limit: u32,
    /// A saved search becomes a smart folder (spec §12).
    pub save_as: Option<CollectionId>,
}

impl SearchRequest {
    pub fn new(query: Query) -> Self {
        Self {
            query,
            scope: None,
            limit: 100,
            save_as: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_facets_and_booleans() {
        let q = parse("super mario AND platform:snes OR title:zelda").unwrap();
        match q {
            Query::Or(parts) => assert_eq!(parts.len(), 2),
            _ => panic!("expected OR"),
        }
    }

    #[test]
    fn rejects_empty() {
        assert!(parse("   ").is_err());
    }

    #[test]
    fn to_fts_renders_text_as_prefix_terms() {
        assert_eq!(to_fts(&Query::Text("hello world".into())), "hello* world*");
        assert_eq!(to_fts(&Query::Text("  padded  ".into())), "padded*");
        // FTS5 special chars are neutralized, never passed through raw.
        assert_eq!(to_fts(&Query::Text(r#"a"b (c:d)"#.into())), "a* b* c* d*");
        assert_eq!(to_fts(&Query::Text("file.txt".into())), "file* txt*");
        assert_eq!(to_fts(&Query::Text("***".into())), "");
    }

    #[test]
    fn to_fts_renders_facets_as_plain_text() {
        // MVP: facet values are plain text terms over the name index.
        let q = Query::Facet {
            field: FacetField::Type,
            op: FacetOp::Eq,
            value: "pdf".into(),
        };
        assert_eq!(to_fts(&q), "pdf*");
    }

    #[test]
    fn to_fts_renders_boolean_combinators() {
        let and = Query::And(vec![
            Query::Text("mario".into()),
            Query::Text("snes".into()),
        ]);
        assert_eq!(to_fts(&and), "mario* snes*");

        let or = Query::Or(vec![
            Query::Text("mario".into()),
            Query::Text("zelda".into()),
        ]);
        assert_eq!(to_fts(&or), "(mario* OR zelda*)");

        // Negations sort after positive terms: FTS5's NOT is binary and
        // needs a left-hand operand (`a* NOT (b*)`).
        let and_not = Query::And(vec![
            Query::Not(Box::new(Query::Text("draft".into()))),
            Query::Text("report".into()),
        ]);
        assert_eq!(to_fts(&and_not), "report* NOT (draft*)");

        // Nested: Or inside And keeps its parens.
        let nested = Query::And(vec![
            Query::Or(vec![
                Query::Text("mario".into()),
                Query::Text("zelda".into()),
            ]),
            Query::Text("snes".into()),
        ]);
        assert_eq!(to_fts(&nested), "(mario* OR zelda*) snes*");
    }

    /// End-to-end: `parse` → `to_fts` → `Catalog::search_fts` must produce
    /// executable FTS5, including the NOT form.
    #[test]
    fn to_fts_output_executes_against_catalog() {
        let cat = ufm_catalog::Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&ufm_vfs::Location {
            id: loc,
            provider: "local".into(),
            display_name: "t".into(),
            capabilities: Default::default(),
            protected: ufm_vfs::ProtectionTier::None,
        })
        .unwrap();
        for name in ["quarterly report.pdf", "draft report.txt", "holiday photos"] {
            cat.insert_item(&ufm_vfs::Item {
                id: ufm_core::ItemId::new(),
                location: loc,
                name: name.into(),
                kind: ufm_vfs::ItemKind::File,
                size_bytes: None,
                modified: None,
                provider_key: Some(format!("/x/{name}")),
                labels: vec![],
                starred: false,
            })
            .unwrap();
        }

        let q = parse("report AND NOT draft").unwrap();
        let fts = to_fts(&q);
        assert_eq!(fts, "report* NOT (draft*)");
        let hits = cat.search_fts(&fts, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.name, "quarterly report.pdf");

        let q = parse("report OR photos").unwrap();
        let hits = cat.search_fts(&to_fts(&q), 10).unwrap();
        assert_eq!(hits.len(), 3);

        let q = parse("type:pdf").unwrap();
        let hits = cat.search_fts(&to_fts(&q), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.name, "quarterly report.pdf");
    }
}
