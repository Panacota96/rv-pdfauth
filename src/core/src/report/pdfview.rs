//! PDF-specific parts of the item reports, built from `ItemResult::details` (the `PdfReport` plus the
//! core's extras: `fingerprint_dbs`, `visual`, `revision_source`) and `ItemResult::outputs`.
//!
//! Two shapes: `*_json` is language-neutral (technical.json / summary.json / statistics.json) and
//! `*_view` is the localized view model shared by the HTML templates and the PDF writer.

use super::ItemResult;
use crate::i18n::{Catalog, catalog, fill};
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Parses the date forms found in PDFs and XMP after normalisation: RFC 3339 with or without seconds,
/// naive date-time (assumed UTC) and bare dates.
pub fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    for f in ["%Y-%m-%dT%H:%M%:z", "%Y-%m-%dT%H:%M:%S%.f%:z"] {
        if let Ok(d) = DateTime::parse_from_str(s, f) {
            return Some(d.with_timezone(&Utc));
        }
    }
    let t = s.trim_end_matches('Z');
    for f in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(d) = NaiveDateTime::parse_from_str(t, f) {
            return Some(d.and_utc());
        }
    }
    NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
}

fn iso(d: &DateTime<Utc>) -> String {
    d.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn is_pdf(it: &ItemResult) -> bool {
    it.details.get("revisions").is_some_and(Value::is_array)
}

fn str_of(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn count(v: &Value) -> usize {
    v.as_array().map_or(0, Vec::len)
}

/// Info dictionary date of a revision: ModDate, else CreationDate (UTC when it parsed, raw otherwise).
fn info_date(rev: &Value) -> Option<String> {
    let pick = |k: &str| {
        let d = &rev["info"][k];
        str_of(&d["utc"]).or_else(|| str_of(&d["raw"]))
    };
    pick("mod_date").or_else(|| pick("creation_date"))
}

fn xmp_date(rev: &Value) -> Option<String> {
    let x = &rev["xmp"];
    let raw = str_of(&x["modify_date"]).or_else(|| str_of(&x["create_date"]))?;
    Some(parse_date(&raw).map(|d| iso(&d)).unwrap_or(raw))
}

/// Creation time of the document: Info CreationDate of revision 1, else XMP CreateDate, else the first save.
fn creation(revs: &[Value]) -> Option<DateTime<Utc>> {
    let r1 = revs.first()?;
    str_of(&r1["info"]["creation_date"]["utc"])
        .and_then(|s| parse_date(&s))
        .or_else(|| str_of(&r1["xmp"]["create_date"]).and_then(|s| parse_date(&s)))
        .or_else(|| saved_at(r1))
}

/// The time a revision was saved, for intervals: Info date first (UTC), else XMP.
fn saved_at(rev: &Value) -> Option<DateTime<Utc>> {
    info_date(rev)
        .and_then(|s| parse_date(&s))
        .or_else(|| xmp_date(rev).and_then(|s| parse_date(&s)))
}

fn producer(rev: &Value) -> Option<String> {
    str_of(&rev["info"]["producer"]).or_else(|| str_of(&rev["xmp"]["producer"]))
}

fn creator(rev: &Value) -> Option<String> {
    str_of(&rev["info"]["creator"]).or_else(|| str_of(&rev["xmp"]["creator_tool"]))
}

/// Language-neutral revision rows.
fn revision_rows(revs: &[Value]) -> Vec<Value> {
    let mut prev: Option<DateTime<Utc>> = None;
    revs.iter()
        .map(|r| {
            let at = saved_at(r);
            let since = match (prev, at) {
                (Some(a), Some(b)) => Some((b - a).num_seconds()),
                _ => None,
            };
            prev = at.or(prev);
            let start = r["start_offset"].as_u64().unwrap_or(0);
            let end = r["end_offset"].as_u64().unwrap_or(0);
            json!({
                "index": r["index"], "start_offset": start, "end_offset": end,
                "bytes": end.saturating_sub(start),
                "saved_at": at.map(|d| iso(&d)), "saved_at_info": info_date(r), "saved_at_xmp": xmp_date(r),
                "since_previous_s": since,
                "producer": producer(r), "creator": creator(r),
                "objects_added": count(&r["objects_added"]), "objects_changed": count(&r["objects_changed"]),
                "objects_deleted": count(&r["objects_deleted"]),
                "pages_changed": r["pages_changed"].as_array().cloned().unwrap_or_default(),
                "signed_by": r["signed_by"].as_array().cloned().unwrap_or_default(),
                "change_kind": r["change_kind"].as_array().cloned().unwrap_or_default(),
                "xref_kind": r["xref_kind"],
            })
        })
        .collect()
}

/// PNG files written by the visual comparison (relative to the item folder).
fn visual_images(it: &ItemResult) -> Vec<String> {
    it.outputs
        .iter()
        .filter(|o| o.kind == "visual_diff")
        .map(|o| o.rel_path.clone())
        .collect()
}

/// `technical.json` → `pdf` (null for items that are not PDFs or whose analysis did not complete).
pub fn pdf_json(it: &ItemResult) -> Value {
    if !is_pdf(it) {
        return Value::Null;
    }
    let d = &it.details;
    let revs = d["revisions"].as_array().cloned().unwrap_or_default();
    let vis = &d["visual"];
    json!({
        "revision_source": d["revision_source"],
        "page_count": d["page_count"],
        "version": d["version"],
        "encrypted": d["encrypted"],
        "revisions": revision_rows(&revs),
        "signatures": d["signatures"].as_array().cloned().unwrap_or_default().iter().map(|s| json!({
            "field": s["field"], "signer_subject": s["signer_subject"], "signing_time": s["signing_time"],
            "revision": s["revision"], "covers_revision": s["covers_revision"], "digest_ok": s["digest_ok"],
            "signature_ok": s["signature_ok"], "docmdp": s["docmdp"], "later_revisions": s["later_revisions"],
            "sub_filter": s["sub_filter"],
        })).collect::<Vec<_>>(),
        "fonts": d["fonts"].as_array().cloned().unwrap_or_default(),
        "fingerprint_dbs": d["fingerprint_dbs"].as_array().cloned().unwrap_or_default(),
        "fingerprint_matches": d["fingerprints"].as_array().cloned().unwrap_or_default(),
        "visual": {
            "status": vis["status"], "reason": vis["reason"], "pairs_compared": vis["pairs_compared"],
            "pages_changed": vis["pages_changed"], "dpi": vis["dpi"], "images": visual_images(it),
        },
    })
}

/// `technical.json` → `extracted_files`: every file the analysis wrote, with its digests.
pub fn extracted_json(it: &ItemResult) -> Value {
    json!(
        it.outputs
            .iter()
            .map(|o| json!({"rel_path": o.rel_path, "kind": o.kind, "bytes": o.bytes, "hashes": o.hashes}))
            .collect::<Vec<_>>()
    )
}

// ---------- plain-language history (customer summary) ----------

/// The facts behind the history paragraph, language-neutral (`summary.json` → `history`).
pub struct History {
    pub saves: usize,
    pub last_change_after_s: Option<i64>,
    /// Category (or label when the category is unknown) of the tool fingerprinted on the last revision.
    pub tool_category: Option<String>,
    pub tool_label: Option<String>,
}

pub fn history(it: &ItemResult) -> Option<History> {
    if !is_pdf(it) {
        return None;
    }
    let revs = it.details["revisions"].as_array()?;
    let saves = revs.len();
    if saves == 0 {
        return None;
    }
    let last = saves;
    let last_change_after_s = if saves > 1 {
        match (creation(revs), revs.last().and_then(saved_at)) {
            (Some(a), Some(b)) if b >= a => Some((b - a).num_seconds()),
            _ => None,
        }
    } else {
        None
    };
    // The tool that made the last save: prefer an editor-type match on the last revision.
    let matches = it.details["fingerprints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let rank = |c: &str| match c {
        "online_editor" | "unlocker" => 0,
        "pdf_editor" => 1,
        _ => 2,
    };
    let best = matches
        .iter()
        .filter(|m| m["revision"].as_u64() == Some(last as u64))
        .min_by_key(|m| rank(m["category"].as_str().unwrap_or("")));
    Some(History {
        saves,
        last_change_after_s,
        tool_category: best.and_then(|m| str_of(&m["category"])),
        tool_label: best.and_then(|m| str_of(&m["label"])),
    })
}

/// "9 days", "3 hours", "less than a minute" in the catalogue's language.
pub fn coarse_duration(c: &Catalog, secs: i64) -> String {
    let (n, one, many) = if secs >= 86_400 {
        (secs / 86_400, "dur_day", "dur_days")
    } else if secs >= 3_600 {
        (secs / 3_600, "dur_hour", "dur_hours")
    } else if secs >= 60 {
        (secs / 60, "dur_minute", "dur_minutes")
    } else {
        return c.label("dur_lt_minute");
    };
    let mut p = BTreeMap::new();
    p.insert("n".to_string(), n.to_string());
    fill(&c.label(if n == 1 { one } else { many }), &p)
}

const TOOL_CATEGORIES: [&str; 8] = [
    "online_editor",
    "pdf_editor",
    "office",
    "pdf_library",
    "scanner",
    "unlocker",
    "printer_driver",
    "browser",
];

pub fn history_text(c: &Catalog, h: &History) -> String {
    let mut p = BTreeMap::new();
    let tool = match (&h.tool_category, &h.tool_label) {
        (Some(cat), _) if TOOL_CATEGORIES.contains(&cat.as_str()) => {
            c.label(&format!("tool_{}", cat))
        }
        (_, Some(label)) => label.clone(),
        _ => String::new(),
    };
    let tool_clause = if tool.is_empty() {
        String::new()
    } else {
        let mut q = BTreeMap::new();
        q.insert("tool".to_string(), tool);
        fill(&c.label("history_with_tool"), &q)
    };
    p.insert("tool".to_string(), tool_clause);
    p.insert("count".to_string(), h.saves.to_string());
    let key = if h.saves <= 1 {
        "history_once"
    } else if let Some(s) = h.last_change_after_s {
        p.insert("elapsed".to_string(), coarse_duration(c, s));
        "history_many"
    } else {
        "history_many_nodate"
    };
    format!("{} {}", fill(&c.label(key), &p), c.label("history_caveat"))
}

pub fn history_json(it: &ItemResult, langs: &[String]) -> Value {
    match history(it) {
        None => Value::Null,
        Some(h) => {
            let text: BTreeMap<String, String> = langs
                .iter()
                .map(|l| (l.clone(), history_text(catalog(l), &h)))
                .collect();
            json!({"saves": h.saves, "last_change_after_s": h.last_change_after_s,
                   "tool_category": h.tool_category, "tool_label": h.tool_label, "text": text})
        }
    }
}

// ---------- localized view model (HTML + PDF) ----------

fn yes_no(c: &Catalog, v: &Value) -> String {
    match v.as_bool() {
        Some(true) => c.label("yes"),
        Some(false) => c.label("no"),
        None => c.label("not_checked"),
    }
}

fn join(v: &Value) -> String {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    x.as_str()
                        .map(String::from)
                        .unwrap_or_else(|| x.to_string())
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn or_dash(s: Option<String>) -> String {
    s.filter(|x| !x.is_empty())
        .unwrap_or_else(|| "\u{2014}".into())
}

/// `pdf` key of the item view model; Null when the item is not an analysed PDF.
pub fn pdf_view(c: &Catalog, it: &ItemResult) -> Value {
    let pj = pdf_json(it);
    if pj.is_null() {
        return Value::Null;
    }
    let revs = pj["revisions"].as_array().cloned().unwrap_or_default();
    let rev_rows: Vec<Value> = revs
        .iter()
        .map(|r| {
            let since = r["since_previous_s"]
                .as_i64()
                .map(|s| {
                    if s < 0 {
                        format!("\u{2212}{}", coarse_duration(c, -s))
                    } else {
                        coarse_duration(c, s)
                    }
                })
                .unwrap_or_else(|| "\u{2014}".into());
            json!({
                "index": r["index"],
                "byte_range": format!("{}\u{2013}{} ({} B)", r["start_offset"], r["end_offset"], r["bytes"]),
                "saved_info": or_dash(str_of(&r["saved_at_info"])),
                "saved_xmp": or_dash(str_of(&r["saved_at_xmp"])),
                "producer": or_dash(str_of(&r["producer"]).or_else(|| str_of(&r["creator"]))),
                "since": since,
                "objects": format!("{} / {} / {}", r["objects_added"], r["objects_changed"], r["objects_deleted"]),
                "pages": or_dash(Some(join(&r["pages_changed"]))),
                "signed_by": or_dash(Some(join(&r["signed_by"]))),
            })
        })
        .collect();

    // Timeline: positions follow the stored dates when every revision has one and they span time;
    // otherwise revisions are spaced evenly (and the note says so).
    let times: Vec<Option<DateTime<Utc>>> = revs
        .iter()
        .map(|r| str_of(&r["saved_at"]).and_then(|s| parse_date(&s)))
        .collect();
    let dated = times.iter().all(Option::is_some) && times.len() > 1;
    let (t0, t1) = if dated {
        let ts: Vec<i64> = times.iter().flatten().map(|t| t.timestamp()).collect();
        (
            *ts.iter().min().unwrap_or(&0),
            *ts.iter().max().unwrap_or(&0),
        )
    } else {
        (0, 0)
    };
    let by_time = dated && t1 > t0;
    let n = revs.len().max(1);
    let timeline: Vec<Value> = revs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let pos = if by_time {
                let t = times[i].map_or(t0, |t| t.timestamp());
                4.0 + 92.0 * (t - t0) as f64 / (t1 - t0) as f64
            } else if n == 1 {
                50.0
            } else {
                4.0 + 92.0 * i as f64 / (n - 1) as f64
            };
            json!({"index": r["index"], "pos": format!("{:.1}", pos), "up": i % 2 == 1,
                   "date": or_dash(str_of(&r["saved_at"]).map(|s| s.replace('T', " ").trim_end_matches('Z').to_string())),
                   "since": rev_rows[i]["since"], "producer": rev_rows[i]["producer"]})
        })
        .collect();

    let sig_rows: Vec<Value> = pj["signatures"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| {
            json!({
                "field": or_dash(str_of(&s["field"])),
                "signer": or_dash(str_of(&s["signer_subject"])),
                "time": or_dash(str_of(&s["signing_time"])),
                "revision": s["revision"].as_u64().map(|r| r.to_string()).unwrap_or_else(|| "\u{2014}".into()),
                "covers": yes_no(c, &s["covers_revision"]),
                "digest": yes_no(c, &s["digest_ok"]),
                "valid": yes_no(c, &s["signature_ok"]),
                "docmdp": s["docmdp"].as_u64().map(|r| r.to_string()).unwrap_or_else(|| "\u{2014}".into()),
                "later": or_dash(Some(join(&s["later_revisions"]))),
            })
        })
        .collect();
    let font_rows: Vec<Value> = pj["fonts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| {
            json!({"name": or_dash(str_of(&f["name"])), "subset": or_dash(str_of(&f["subset_prefix"])),
                   "embedded": yes_no(c, &f["embedded"]), "first_revision": f["first_revision"],
                   "object": or_dash(str_of(&f["object"]))})
        })
        .collect();
    // Scale rule: an item report must not grow without bound; the full list is in objects.json.
    const MAX_FONT_ROWS: usize = 100;
    let mut font_rows = font_rows;
    if font_rows.len() > MAX_FONT_ROWS {
        let more = font_rows.len() - MAX_FONT_ROWS;
        font_rows.truncate(MAX_FONT_ROWS);
        font_rows.push(json!({"name": format!("\u{2026} (+{more}, objects.json)"), "subset": "\u{2014}",
                              "embedded": "\u{2014}", "first_revision": "\u{2014}", "object": "\u{2014}"}));
    }
    let db_rows: Vec<Value> = pj["fingerprint_dbs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|d| {
            json!({"role": or_dash(str_of(&d["role"])), "name": or_dash(str_of(&d["name"])),
                   "version": or_dash(str_of(&d["version"])),
                   "entries": d["entries"].as_u64().map(|n| n.to_string()).unwrap_or_else(|| "\u{2014}".into()),
                   "sha256": or_dash(str_of(&d["sha256"])), "status": or_dash(str_of(&d["status"]))})
        })
        .collect();
    let match_rows: Vec<Value> = pj["fingerprint_matches"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            json!({"revision": m["revision"], "label": or_dash(str_of(&m["label"])),
                   "category": or_dash(str_of(&m["category"])), "matched_on": or_dash(str_of(&m["matched_on"])),
                   "value": or_dash(str_of(&m["matched_value"])), "db": or_dash(str_of(&m["db"])),
                   "mismatches": or_dash(Some(join(&m["trait_mismatches"])))})
        })
        .collect();
    let vis = &pj["visual"];
    let images: Vec<Value> = vis["images"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|p| {
            let p = p.as_str().unwrap_or_default();
            let name = p.rsplit('/').next().unwrap_or(p);
            json!({"href": p, "name": name})
        })
        .collect();
    let vis_status = str_of(&vis["status"]).unwrap_or_else(|| "not_applicable".into());
    json!({
        "revision_rows": rev_rows,
        "timeline": timeline,
        "timeline_by_time": by_time,
        "signature_rows": sig_rows,
        "font_rows": font_rows,
        "db_rows": db_rows,
        "match_rows": match_rows,
        "visual": {
            "status": vis_status,
            "status_label": c.label(&format!("visual_{}", vis_status)),
            "reason": str_of(&vis["reason"]).unwrap_or_default(),
            "pairs_compared": vis["pairs_compared"].as_u64().unwrap_or(0),
            "pages_changed": vis["pages_changed"].as_u64().unwrap_or(0),
            "images": images,
        },
        "history_text": history(it).map(|h| history_text(c, &h)).unwrap_or_default(),
    })
}

/// Localized rows for every file the analysis wrote (empty for items without outputs).
pub fn extracted_view(it: &ItemResult) -> Value {
    json!(
        it.outputs
            .iter()
            .map(|o| json!({
                "href": o.rel_path, "kind": o.kind, "bytes": o.bytes,
                "digests": o.hashes.iter().map(|(a, h)| format!("{}: {}", a, h)).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>()
    )
}

// ---------- statistics ----------

/// Run-level PDF counts: analysed PDFs, PDFs with more than one revision, with signatures, with an
/// online-editor fingerprint.
pub fn pdf_overview(items: &[ItemResult]) -> Value {
    let (mut pdfs, mut multi, mut signed, mut online) = (0u64, 0u64, 0u64, 0u64);
    for it in items.iter().filter(|it| is_pdf(it)) {
        pdfs += 1;
        if count(&it.details["revisions"]) > 1 {
            multi += 1;
        }
        if count(&it.details["signatures"]) > 0 {
            signed += 1;
        }
        if it.details["fingerprints"]
            .as_array()
            .is_some_and(|a| a.iter().any(|m| m["category"] == "online_editor"))
        {
            online += 1;
        }
    }
    json!({"pdfs": pdfs, "multi_revision": multi, "signed": signed, "online_editor": online})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pdf_and_xmp_date_forms() {
        for s in [
            "2026-10-01T10:00:00Z",
            "2026-10-01T12:00:00+02:00",
            "2026-10-01T12:00+02:00",
            "2026-10-01T10:00:00",
        ] {
            assert_eq!(
                iso(&parse_date(s).unwrap()),
                "2026-10-01T10:00:00Z",
                "{}",
                s
            );
        }
        assert!(parse_date("2026-10-01").is_some());
        assert!(parse_date("yesterday").is_none());
    }

    fn item(details: Value) -> ItemResult {
        ItemResult {
            index: 1,
            path: "a.pdf".into(),
            size: 10,
            modified_utc: None,
            hashes: vec![],
            detected_type: None,
            extension: None,
            findings: vec![],
            error: None,
            details,
            outputs: vec![],
            audit_first_seq: 1,
            audit_last_seq: 2,
        }
    }

    #[test]
    fn history_paragraph_in_three_languages() {
        let rev = |i: u64, start: u64, end: u64, date: &str| {
            json!({"index": i, "start_offset": start, "end_offset": end,
                   "info": {"creation_date": {"raw": "D:20261001", "utc": "2026-10-01T08:00:00Z"},
                            "mod_date": {"raw": "x", "utc": date}}})
        };
        let it = item(json!({
            "is_pdf": true,
            "revisions": [rev(1, 0, 100, "2026-10-01T08:00:00Z"), rev(2, 100, 200, "2026-10-05T08:00:00Z"),
                          rev(3, 200, 300, "2026-10-10T09:00:00Z")],
            "fingerprints": [{"revision": 3, "category": "online_editor", "label": "Smallpdf"},
                             {"revision": 1, "category": "office", "label": "Word"}],
        }));
        let h = history(&it).unwrap();
        assert_eq!(
            history_text(catalog("en"), &h),
            "The document was saved 3 times; the last change was made 9 days after creation, with an \
             online editor. This information comes from the document itself and is not proof on its own."
        );
        assert!(history_text(catalog("es"), &h).starts_with(
            "El documento se guardó 3 veces; el último cambio se hizo 9 días después de su creación, con un editor en línea."
        ));
        assert!(
            history_text(catalog("fr"), &h)
                .contains("9 jours après sa création, avec un éditeur en ligne")
        );
        let v = pdf_view(catalog("en"), &it);
        assert_eq!(v["revision_rows"][1]["since"], "4 days");
        assert_eq!(v["timeline"][0]["pos"], "4.0");
        assert_eq!(v["timeline"][2]["pos"], "96.0");
        assert!(pdf_view(catalog("en"), &item(Value::Null)).is_null());
        let ov = pdf_overview(&[it]);
        assert_eq!(ov["multi_revision"], 1);
        assert_eq!(ov["online_editor"], 1);
    }

    #[test]
    fn coarse_durations_are_localized() {
        assert_eq!(coarse_duration(catalog("en"), 9 * 86_400 + 5), "9 days");
        assert_eq!(coarse_duration(catalog("en"), 3_600), "1 hour");
        assert_eq!(coarse_duration(catalog("es"), 2 * 86_400), "2 días");
        assert_eq!(
            coarse_duration(catalog("fr"), 30),
            "moins d\u{2019}une minute"
        );
    }
}
