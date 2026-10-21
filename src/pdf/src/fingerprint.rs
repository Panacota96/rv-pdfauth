//! Producer / editor fingerprints: the bundled starter database plus databases passed in
//! `PdfOptions::fingerprints`. Matching is case-insensitive substring search on Producer,
//! Creator, XMP pdf:Producer and XMP xmp:CreatorTool; structural traits are then compared with
//! what the revision actually looks like.

use crate::model::{FingerprintDb, FingerprintEntry, FingerprintMatch, InfoDict, Xmp};
use std::collections::{BTreeMap, HashSet};

const STARTER: &str = include_str!("../../../fingerprints/starter.json");

/// The bundled starter database (fingerprints/starter.json).
pub fn starter_db() -> FingerprintDb {
    serde_json::from_str(STARTER).unwrap_or_default()
}

/// Structural traits of one revision, as compared with `FingerprintEntry::traits`.
#[derive(Clone, Debug, Default)]
pub struct RevTraits {
    pub revision: usize,
    /// key → observed value. Header traits are only present for revision 1.
    pub values: BTreeMap<String, String>,
}

pub struct Strings<'a> {
    pub revision: usize,
    pub info: &'a InfoDict,
    pub xmp: Option<&'a Xmp>,
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    !needle.trim().is_empty() && hay.to_lowercase().contains(&needle.to_lowercase())
}

fn trait_mismatches(e: &FingerprintEntry, t: &RevTraits) -> Vec<String> {
    let mut out = Vec::new();
    for (k, want) in &e.traits {
        let Some(have) = t.values.get(k) else {
            continue; // not observable for this revision (e.g. header traits after revision 1)
        };
        let ok = if k.ends_with("_contains") {
            contains_ci(have, want)
        } else {
            have.eq_ignore_ascii_case(want)
        };
        if !ok {
            let have = if have.trim().is_empty() {
                "(none)"
            } else {
                have.as_str()
            };
            out.push(format!("{}: expected {}, found {}", k, want, have));
        }
    }
    out
}

/// Matches every revision's metadata strings against the databases. One match per
/// (database, entry, field, string): later revisions repeating the same string are not repeated.
pub fn match_all(
    dbs: &[FingerprintDb],
    revs: &[Strings],
    traits: &[RevTraits],
) -> Vec<FingerprintMatch> {
    let mut out = Vec::new();
    let mut seen: HashSet<(String, String, String, String)> = HashSet::new();
    for s in revs {
        let t = traits
            .iter()
            .find(|t| t.revision == s.revision)
            .cloned()
            .unwrap_or_default();
        let fields: [(&str, Option<&String>, bool); 4] = [
            ("producer", s.info.producer.as_ref(), true),
            ("creator", s.info.creator.as_ref(), false),
            (
                "xmp_producer",
                s.xmp.and_then(|x| x.producer.as_ref()),
                true,
            ),
            (
                "xmp_creator_tool",
                s.xmp.and_then(|x| x.creator_tool.as_ref()),
                false,
            ),
        ];
        for db in dbs {
            for e in &db.entries {
                for (field, val, is_prod) in fields {
                    let Some(v) = val else { continue };
                    let subs = if is_prod {
                        &e.producer_contains
                    } else {
                        &e.creator_contains
                    };
                    if !subs.iter().any(|n| contains_ci(v, n)) {
                        continue;
                    }
                    let key = (db.name.clone(), e.id.clone(), field.to_string(), v.clone());
                    if !seen.insert(key) {
                        continue;
                    }
                    out.push(FingerprintMatch {
                        revision: s.revision,
                        db: db.name.clone(),
                        entry_id: e.id.clone(),
                        label: e.label.clone(),
                        category: e.category.clone(),
                        matched_on: field.to_string(),
                        trait_mismatches: trait_mismatches(e, &t),
                        matched_value: v.clone(),
                    });
                }
            }
        }
    }
    // Structure-only identification from the header comment (revision 1).
    if let Some(t1) = traits.iter().find(|t| t.revision == 1)
        && let Some(hc) = t1.values.get("header_comment_contains")
    {
        for db in dbs {
            for e in &db.entries {
                let Some(want) = e.traits.get("header_comment_contains") else {
                    continue;
                };
                let already = out.iter().any(|m| m.entry_id == e.id && m.db == db.name);
                if !already && contains_ci(hc, want) {
                    out.push(FingerprintMatch {
                        revision: 1,
                        db: db.name.clone(),
                        entry_id: e.id.clone(),
                        label: e.label.clone(),
                        category: e.category.clone(),
                        matched_on: "structure".into(),
                        trait_mismatches: trait_mismatches(e, t1),
                        matched_value: hc.clone(),
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_db_is_valid() {
        let db = starter_db();
        assert!(db.entries.len() >= 25, "{} entries", db.entries.len());
        let cats: HashSet<&str> = db.entries.iter().map(|e| e.category.as_str()).collect();
        for c in [
            "office",
            "pdf_library",
            "scanner",
            "pdf_editor",
            "online_editor",
            "unlocker",
            "printer_driver",
            "browser",
        ] {
            assert!(cats.contains(c), "missing category {}", c);
        }
        let mut ids = HashSet::new();
        for e in &db.entries {
            assert!(ids.insert(e.id.clone()), "duplicate id {}", e.id);
            assert!(!e.source.trim().is_empty(), "{} has no source", e.id);
            assert!(!e.label.trim().is_empty());
            assert!(!e.producer_contains.is_empty() || !e.creator_contains.is_empty());
        }
    }

    #[test]
    fn matches_case_insensitively_and_checks_traits() {
        let db = FingerprintDb {
            name: "t".into(),
            version: "1".into(),
            entries: vec![FingerprintEntry {
                id: "rl".into(),
                label: "ReportLab".into(),
                category: "pdf_library".into(),
                producer_contains: vec!["reportlab".into()],
                traits: [("xref_kind".to_string(), "table".to_string())].into(),
                ..Default::default()
            }],
        };
        let info = InfoDict {
            producer: Some("ReportLab PDF Library".into()),
            ..Default::default()
        };
        let t = RevTraits {
            revision: 1,
            values: [("xref_kind".to_string(), "stream".to_string())].into(),
        };
        let m = match_all(
            &[db],
            &[Strings {
                revision: 1,
                info: &info,
                xmp: None,
            }],
            &[t],
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].trait_mismatches.len(), 1);
    }
}
