//! ufm-search: the query language and index feeds.
//!
//! Spec §13: X1-class background indexing. One search language everywhere —
//! keywords, labels, types, dates, sizes, Boolean operators, inside-file
//! content — with results that update as the query is typed. Saved searches
//! power smart folders. Sub-100ms feel on large libraries comes from the
//! catalog's FTS tables, never from live tree-walking.

use serde::{Deserialize, Serialize};
use ufm_core::{CollectionId, LocationId, Result, UfmError};

pub mod census;
pub mod indexer;
pub mod scope;
pub use census::{census_root, Census, TopEntry, GATE_THRESHOLD_FILES};
pub use indexer::{Indexer, IndexStats};
pub use scope::IndexScope;

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
    /// `starred:yes` — user metadata, lives in the catalog (path_stars),
    /// not in the name index; extracted and post-filtered by the caller.
    Starred,
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
            "starred" | "star" => FacetField::Starred,
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
///   `Not` parts are rendered after positive parts so the result is of the
///   form `a* b* NOT (c*)`, which FTS5 accepts. An `And` with NO positive
///   parts renders as the empty string (match nothing) — FTS5's `NOT` is
///   binary and a bare `NOT (c*)` is a syntax error.
/// - `Or(parts)` → non-empty parts joined with ` OR `, wrapped in
///   parentheses: `(a* OR b*)`. (`Not` parts render empty and drop out;
///   OR-negation is a later milestone.)
/// - `Not(q)` at the top level → the empty string (match nothing): FTS5 has
///   no unary `-term` operator (a leading `-` is parsed as a column filter
///   and fails) and no bare `NOT`, so there is nothing valid to render.
///
/// Words are split on whitespace and on every FTS5 special character
/// (`" * ( ) : { } [ ] ^ + -` and friends); empty fragments are dropped, so
/// the result is always a syntactically safe MATCH expression — or the
/// empty string when nothing renderable remains (which `search_fts` treats
/// as "no rows").
pub fn to_fts(q: &Query) -> String {
    match q {
        Query::Text(s) => words(s).join(" "),
        // Label/Starred facets are user metadata, not name text: they
        // render EMPTY here and are executed by the caller as catalog
        // post-filters (see metadata_facets). Other facets keep the
        // MVP plain-text-over-name rendering.
        Query::Facet {
            field: FacetField::Label | FacetField::Starred,
            ..
        } => String::new(),
        Query::Facet { value, .. } => words(value).join(" "),
        Query::And(parts) => {
            // Positive parts first, negations last: FTS5's NOT is binary and
            // needs a left-hand operand.
            let (negs, poss): (Vec<&Query>, Vec<&Query>) =
                parts.iter().partition(|p| matches!(p, Query::Not(_)));
            let mut out: Vec<String> = poss.iter().map(|p| to_fts(p)).filter(|s| !s.is_empty()).collect();
            if out.is_empty() {
                // No positive term for NOT to bind to — invalid FTS5.
                // Match nothing rather than error.
                return String::new();
            }
            out.extend(negs.iter().filter_map(|p| match p {
                Query::Not(inner) => {
                    let s = to_fts(inner);
                    if s.is_empty() {
                        None
                    } else {
                        Some(format!("NOT ({s})"))
                    }
                }
                _ => None, // unreachable: partitioned above
            }));
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
        // Bare NOT: no left operand exists in FTS5 — match nothing.
        Query::Not(_) => String::new(),
    }
}

/// Collect the positive user-metadata facets of a query: `label:`
/// values (AND semantics — an item must carry every label) and the
/// `starred:` expectation (`Some(true)` / `Some(false)`), gathered
/// from the top-level And chain only. Facets inside Or/Not are NOT
/// extracted (they keep their legacy rendering); a negated metadata
/// facet is a documented non-feature of the v1 language.
pub fn metadata_facets(q: &Query) -> (Vec<String>, Option<bool>) {
    fn walk(q: &Query, labels: &mut Vec<String>, starred: &mut Option<bool>) {
        match q {
            Query::And(parts) => {
                for p in parts {
                    walk(p, labels, starred);
                }
            }
            Query::Facet {
                field: FacetField::Label,
                value,
                ..
            } => {
                if !value.is_empty() {
                    labels.push(value.clone());
                }
            }
            Query::Facet {
                field: FacetField::Starred,
                value,
                ..
            } => {
                let v = value.to_lowercase();
                *starred = Some(!matches!(v.as_str(), "no" | "false" | "0" | "off"));
            }
            _ => {}
        }
    }
    let (mut labels, mut starred) = (Vec::new(), None);
    walk(q, &mut labels, &mut starred);
    (labels, starred)
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

    #[test]
    fn to_fts_bare_not_matches_nothing() {
        // A bare NOT has no left operand — invalid FTS5. It must render as
        // the empty string (which search_fts treats as "no rows"), never as
        // `NOT (x*)`, which would make the query fail.
        let bare = Query::Not(Box::new(Query::Text("draft".into())));
        assert_eq!(to_fts(&bare), "");
        // Same for an AND of only negations: nothing for NOT to bind to.
        let and_only_negs = Query::And(vec![
            Query::Not(Box::new(Query::Text("a".into()))),
            Query::Not(Box::new(Query::Text("b".into()))),
        ]);
        assert_eq!(to_fts(&and_only_negs), "");
        // A negation whose inner renders empty is dropped, not emitted bare.
        let and_empty_neg = Query::And(vec![
            Query::Text("report".into()),
            Query::Not(Box::new(Query::Text("***".into()))),
        ]);
        assert_eq!(to_fts(&and_empty_neg), "report*");
    }

    #[test]
    fn to_fts_bare_not_executes_as_no_rows() {
        // End-to-end: a bare NOT must not error against a real FTS5 index.
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
        cat.insert_item(&ufm_vfs::Item {
            id: ufm_core::ItemId::new(),
            location: loc,
            name: "quarterly report.pdf".into(),
            kind: ufm_vfs::ItemKind::File,
            size_bytes: None,
            modified: None,
            provider_key: Some("/x/quarterly report.pdf".into()),
            labels: vec![],
            starred: false,
        })
        .unwrap();
        let bare = Query::Not(Box::new(Query::Text("quarterly".into())));
        let rendered = to_fts(&bare);
        assert_eq!(rendered, "");
        let hits = cat.search_fts(&rendered, 10).unwrap();
        assert!(hits.is_empty());
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

    #[test]
    fn starred_facet_parses_and_extracts() {
        let q = parse("starred:yes").unwrap();
        assert_eq!(metadata_facets(&q), (vec![], Some(true)));
        let q = parse("star:no").unwrap();
        assert_eq!(metadata_facets(&q), (vec![], Some(false)));
        let q = parse("label:taxes AND starred:yes AND report").unwrap();
        assert_eq!(
            metadata_facets(&q),
            (vec!["taxes".to_string()], Some(true))
        );
    }

    #[test]
    fn metadata_facets_render_empty_in_fts() {
        // A label-only query must NOT degrade to a name search.
        assert_eq!(to_fts(&parse("label:taxes").unwrap()), "");
        assert_eq!(to_fts(&parse("starred:yes").unwrap()), "");
        // Mixed: the text part survives, the metadata parts drop out.
        assert_eq!(
            to_fts(&parse("label:taxes AND report").unwrap()),
            "report*"
        );
        // Negated metadata facets are not extracted (documented v1 limit).
        let q = parse("report AND NOT label:draft").unwrap();
        assert_eq!(metadata_facets(&q).0.len(), 0);
    }
}
