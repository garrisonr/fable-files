//! ufm-search: the query language and index feeds.
//!
//! Spec §13: X1-class background indexing. One search language everywhere —
//! keywords, labels, types, dates, sizes, Boolean operators, inside-file
//! content — with results that update as the query is typed. Saved searches
//! power smart folders. Sub-100ms feel on large libraries comes from the
//! catalog's FTS tables, never from live tree-walking.

use serde::{Deserialize, Serialize};
use ufm_core::{CollectionId, LocationId, Result, UfmError};

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
}
