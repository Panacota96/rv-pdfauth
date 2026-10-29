//! Everything the window shows and every rule it enforces, without any drawing code.
//! Kept separate from `app.rs` so it can be unit-tested headless (CI has no display).

use rv_pdfauth_core::analyzer::{DomainOptions, Finding, Isolation, Severity};
use rv_pdfauth_core::report::ItemResult;
use rv_pdfauth_core::{
    CASE_ROLE, Event, HashAlgo, RunConfig, RunSummary, TOOL_NAME, TOOL_VERSION, case, hash, i18n,
    time::human_duration,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

/// Accepted ranges for the numeric analysis options (values outside are clamped, never refused).
pub const DPI_RANGE: RangeInclusive<u32> = 36..=600;
pub const MAX_PAGES_RANGE: RangeInclusive<u32> = 1..=2000;
pub const TIMEOUT_RANGE: RangeInclusive<u64> = 10..=3600;
pub const MEMORY_RANGE: RangeInclusive<u64> = 256..=65536;

/// One change in section "3. Options"; each maps 1:1 onto a [`DomainOptions`] field and is audited as
/// `ui_<name>_changed` (see [`OptionChange::audit_name`]).
#[derive(Clone, Debug, PartialEq)]
pub enum OptionChange {
    ExtractRevisions(bool),
    VisualDiff(bool),
    Dpi(u32),
    MaxPages(u32),
    ObjectsDump(bool),
    ExtractEmbedded(bool),
    WorkerTimeout(u64),
    WorkerMemory(u64),
    /// Extra fingerprint database (JSON); empty = none.
    FingerprintsDb(String),
    /// true = item reports only for flagged items, false = for every item.
    ItemReportsFlaggedOnly(bool),
}

impl OptionChange {
    pub fn audit_name(&self) -> &'static str {
        match self {
            OptionChange::ExtractRevisions(_) => "ui_extract_revisions_changed",
            OptionChange::VisualDiff(_) => "ui_visual_diff_changed",
            OptionChange::Dpi(_) => "ui_dpi_changed",
            OptionChange::MaxPages(_) => "ui_max_pages_changed",
            OptionChange::ObjectsDump(_) => "ui_objects_dump_changed",
            OptionChange::ExtractEmbedded(_) => "ui_extract_embedded_changed",
            OptionChange::WorkerTimeout(_) => "ui_worker_timeout_changed",
            OptionChange::WorkerMemory(_) => "ui_worker_memory_changed",
            OptionChange::FingerprintsDb(_) => "ui_fingerprints_db_changed",
            OptionChange::ItemReportsFlaggedOnly(_) => "ui_item_reports_changed",
        }
    }
}

/// One row of the results table shown after a run.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultRow {
    pub index: u64,
    pub name: String,
    /// "ok", "attention" or "problem" (as in the reports).
    pub status: String,
    pub findings: usize,
    pub top: Option<Severity>,
    pub revisions: usize,
    /// Case folder only: the item's evidence id (E0001…) from the registry.
    pub evidence_id: Option<String>,
    /// Position in [`Viewer::items`] when the item has something to review (two or more revisions).
    pub viewer_item: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Running,
    Finished,
    Cancelled,
    Failed(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub elapsed_ms: u64,
    pub eta_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub items: u64,
    pub errors: u64,
    pub high: u64,
    pub medium: u64,
    pub low: u64,
    pub info: u64,
    pub report: PathBuf,
    pub log_dir: PathBuf,
}

/// One item's link to the case registry, shown in the result panel.
#[derive(Clone, Debug, PartialEq)]
pub struct EvidenceLink {
    pub index: u64,
    pub path: String,
    pub evidence_id: Option<String>,
    /// registered | already_registered | verified | mismatch | not_in_registry
    pub status: String,
}

pub const LANG_CHOICES: [(&str, &str); 4] = [
    ("en", "English"),
    ("es", "Español"),
    ("fr", "Français"),
    ("all", "All three (EN + ES + FR)"),
];

#[derive(Clone, Debug)]
pub struct GuiState {
    pub inputs: Vec<PathBuf>,
    pub path_entry: String,
    pub recursive: bool,
    pub selected: Vec<HashAlgo>,
    pub lang: String,
    pub out_dir: String,
    pub actor: String,
    pub case_id: String,
    pub phase: Phase,
    pub progress: Progress,
    pub current_item: String,
    pub warnings: Vec<String>,
    pub outcome: Option<Outcome>,
    /// User actions recorded before the run; handed to the core as audit preamble entries.
    pub ui_actions: Vec<(String, Value)>,
    /// Interface language (en/es/fr), independent from the report language.
    pub ui_lang: String,
    pub jurisdiction: String,
    pub timestamp: bool,
    pub tsa_url: String,
    pub sign_cert: String,
    /// Kept in memory only; never logged.
    pub sign_password: String,
    /// Section "3. Options" (isolation is set by [`GuiState::to_config`], not by the user).
    pub options: DomainOptions,
    /// Text of the fingerprint DB field while it is being edited (applied on focus loss / Browse).
    pub fp_db_entry: String,
    pub results: Vec<ResultRow>,
    pub viewer: Viewer,
    /// Shared case folder; when set, the output folder is derived from it and locked.
    pub case_dir: String,
    /// Evidence ids reported by the last run (case folder only).
    pub evidence: Vec<EvidenceLink>,
    /// Case folder only: register the evidence with RV HashInv before analysing (`--intake hashinv`).
    pub intake_hashinv: bool,
    /// Result of looking for rv-hashinv when the option was switched on (path, version, SHA-256) or why not.
    pub hashinv: Option<Result<rv_pdfauth_core::intake::IntakeTool, String>>,
    /// Intake progress of the running job: (done, total).
    pub intake_progress: Option<(u64, u64)>,
    /// Where rv-hashinv wrote its hash reports in the last run.
    pub intake_report_dir: Option<String>,
}

impl Default for GuiState {
    fn default() -> Self {
        GuiState {
            inputs: vec![],
            path_entry: String::new(),
            recursive: true,
            selected: vec![HashAlgo::Sha256],
            lang: "en".into(),
            out_dir: "rakvor-out".into(),
            actor: rv_pdfauth_core::env::os_user(),
            case_id: String::new(),
            phase: Phase::Idle,
            progress: Progress::default(),
            current_item: String::new(),
            warnings: vec![],
            outcome: None,
            ui_actions: vec![],
            ui_lang: "en".into(),
            jurisdiction: "co".into(),
            timestamp: true,
            tsa_url: rv_pdfauth_core::trust::DEFAULT_TSA.into(),
            sign_cert: String::new(),
            sign_password: String::new(),
            options: DomainOptions::default(),
            fp_db_entry: String::new(),
            results: vec![],
            viewer: Viewer::default(),
            case_dir: String::new(),
            evidence: vec![],
            intake_hashinv: false,
            hashinv: None,
            intake_progress: None,
            intake_report_dir: None,
        }
    }
}

impl GuiState {
    /// Records a user action with the time it happened (written to the audit log when the run starts).
    fn note(&mut self, action: &str, mut details: Value) {
        if let Some(o) = details.as_object_mut() {
            o.insert("at_utc".into(), json!(rv_pdfauth_core::time::utc_now()));
        }
        self.ui_actions.push((action.to_string(), details));
    }

    pub fn is_running(&self) -> bool {
        self.phase == Phase::Running
    }

    /// A checkbox is clickable if it can be unticked (more than one ticked) or ticked (fewer than two ticked).
    pub fn can_toggle(&self, a: HashAlgo) -> bool {
        if self.selected.contains(&a) {
            self.selected.len() > 1
        } else {
            self.selected.len() < HashAlgo::MAX_SELECTED
        }
    }

    pub fn toggle_algo(&mut self, a: HashAlgo) -> Result<(), String> {
        if !self.can_toggle(a) {
            return Err(if self.selected.contains(&a) {
                "At least one hash algorithm is required.".into()
            } else {
                format!(
                    "At most {} hash algorithms can be selected.",
                    HashAlgo::MAX_SELECTED
                )
            });
        }
        if let Some(i) = self.selected.iter().position(|x| *x == a) {
            self.selected.remove(i);
        } else {
            self.selected.push(a);
        }
        self.note(
            "ui_hash_selection_changed",
            json!({"algorithms": self.selected}),
        );
        Ok(())
    }

    pub fn add_input(&mut self, p: PathBuf) {
        if !self.inputs.contains(&p) {
            self.note("ui_input_added", json!({"path": p.display().to_string()}));
            self.inputs.push(p);
        }
    }

    pub fn add_typed_path(&mut self) {
        let t = self.path_entry.trim().trim_matches('"').to_string();
        if !t.is_empty() {
            self.add_input(PathBuf::from(t));
            self.path_entry.clear();
        }
    }

    pub fn remove_input(&mut self, i: usize) {
        if i < self.inputs.len() {
            let p = self.inputs.remove(i);
            self.note("ui_input_removed", json!({"path": p.display().to_string()}));
        }
    }

    pub fn set_lang(&mut self, l: &str) {
        if self.lang != l {
            self.lang = l.to_string();
            self.note("ui_language_changed", json!({"lang": l}));
        }
    }

    pub fn set_recursive(&mut self, r: bool) {
        if self.recursive != r {
            self.recursive = r;
            self.note("ui_recursive_changed", json!({"recursive": r}));
        }
    }

    /// GUI text in the interface language.
    pub fn tr(&self, key: &str) -> String {
        i18n::catalog(&self.ui_lang).ui(key)
    }

    pub fn set_ui_lang(&mut self, l: &str) {
        if self.ui_lang != l && ["en", "es", "fr"].contains(&l) {
            self.ui_lang = l.to_string();
            self.note("ui_interface_language_changed", json!({"ui_lang": l}));
        }
    }

    pub fn set_jurisdiction(&mut self, j: &str) {
        if self.jurisdiction != j && (j == "co" || j == "fr") {
            self.jurisdiction = j.to_string();
            self.note("ui_jurisdiction_changed", json!({"jurisdiction": j}));
        }
    }

    pub fn set_timestamp(&mut self, on: bool, url: &str) {
        let url = url.trim().to_string();
        if self.timestamp != on || self.tsa_url != url {
            self.timestamp = on;
            self.tsa_url = url.clone();
            self.note(
                "ui_timestamp_changed",
                json!({"enabled": on, "tsa_url": url}),
            );
        }
    }

    pub fn set_sign_cert(&mut self, path: &str) {
        let path = path.trim().trim_matches('"').to_string();
        if self.sign_cert != path {
            self.sign_cert = path.clone();
            self.note(
                "ui_signing_certificate_changed",
                json!({"path": if path.is_empty() { Value::Null } else { json!(path) }}),
            );
        }
    }

    /// Applies one option change (numbers clamped to their range) and audits it when the value changed.
    /// Returns whether anything changed.
    pub fn set_option(&mut self, c: OptionChange) -> bool {
        fn put<T: PartialEq>(slot: &mut T, v: T) -> bool {
            let changed = *slot != v;
            *slot = v;
            changed
        }
        let o = &mut self.options;
        let (changed, value): (bool, Value) = match &c {
            OptionChange::ExtractRevisions(v) => (put(&mut o.extract_revisions, *v), json!(v)),
            OptionChange::VisualDiff(v) => (put(&mut o.visual_diff, *v), json!(v)),
            OptionChange::Dpi(v) => {
                let v = (*v).clamp(*DPI_RANGE.start(), *DPI_RANGE.end());
                (put(&mut o.dpi, v), json!(v))
            }
            OptionChange::MaxPages(v) => {
                let v = (*v).clamp(*MAX_PAGES_RANGE.start(), *MAX_PAGES_RANGE.end());
                (put(&mut o.max_pages, v), json!(v))
            }
            OptionChange::ObjectsDump(v) => (put(&mut o.objects_dump, *v), json!(v)),
            OptionChange::ExtractEmbedded(v) => (put(&mut o.extract_embedded, *v), json!(v)),
            OptionChange::WorkerTimeout(v) => {
                let v = (*v).clamp(*TIMEOUT_RANGE.start(), *TIMEOUT_RANGE.end());
                (put(&mut o.worker_timeout_s, v), json!(v))
            }
            OptionChange::WorkerMemory(v) => {
                let v = (*v).clamp(*MEMORY_RANGE.start(), *MEMORY_RANGE.end());
                (put(&mut o.worker_memory_mb, v), json!(v))
            }
            OptionChange::FingerprintsDb(p) => {
                let p = p.trim().trim_matches('"').to_string();
                let new = (!p.is_empty()).then(|| PathBuf::from(&p));
                self.fp_db_entry = p.clone();
                (
                    put(&mut o.fingerprints_db, new),
                    if p.is_empty() { Value::Null } else { json!(p) },
                )
            }
            OptionChange::ItemReportsFlaggedOnly(v) => (
                put(&mut o.item_reports_flagged_only, *v),
                json!(if *v { "flagged" } else { "all" }),
            ),
        };
        if changed {
            self.note(c.audit_name(), json!({"value": value}));
        }
        changed
    }

    /// The analysis options handed to the core: the user's choices, with the PDF analysis in a worker
    /// process (this executable started with `worker`).
    pub fn domain_options(&self) -> DomainOptions {
        DomainOptions {
            isolation: std::env::current_exe()
                .map(|exe| Isolation::Subprocess { exe })
                .unwrap_or_default(),
            ..self.options.clone()
        }
    }

    /// Short form of the options, for the "command line" in environment.json and `ui_start_clicked`.
    pub fn options_json(&self) -> Value {
        let o = &self.options;
        json!({"extract_revisions": o.extract_revisions, "visual_diff": o.visual_diff, "dpi": o.dpi,
            "max_pages": o.max_pages, "objects_dump": o.objects_dump, "extract_embedded": o.extract_embedded,
            "worker_timeout_s": o.worker_timeout_s, "worker_memory_mb": o.worker_memory_mb,
            "fingerprints_db": o.fingerprints_db.as_ref().map(|p| p.display().to_string()),
            "item_reports": if o.item_reports_flagged_only { "flagged" } else { "all" }})
    }

    /// Rule: with a case folder the output folder is `<case>/runs/<tool>/<RUN-ID>` and cannot be edited.
    pub fn set_case_dir(&mut self, path: &str) {
        let path = path.trim().trim_matches('"').to_string();
        if self.case_dir != path {
            self.case_dir = path.clone();
            self.note(
                "ui_case_dir_changed",
                json!({"path": if path.is_empty() { Value::Null } else { json!(path) }, "role": CASE_ROLE.name()}),
            );
        }
    }

    /// The case folder given by RV Hub (`--case-dir`): preset like a typed folder (`ui_case_dir_changed`) and
    /// recorded as `ui_case_dir_from_launcher`, so the audit log shows the folder was not chosen in the GUI.
    pub fn set_case_dir_from_launcher(&mut self, path: &std::path::Path) {
        let shown = path.display().to_string();
        self.note("ui_case_dir_from_launcher", json!({"path": shown}));
        self.set_case_dir(&shown);
    }

    /// A launch argument that was ignored (e.g. `--case-dir` without a folder), recorded in the audit log.
    pub fn note_launch_problem(&mut self, problem: &str) {
        self.note("ui_launch_argument_ignored", json!({"problem": problem}));
    }

    /// Switches the RV HashInv intake on or off; switching it on looks for rv-hashinv right away so the
    /// window can say which copy will run (or why none can).
    pub fn set_intake_hashinv(&mut self, on: bool) {
        if self.intake_hashinv == on {
            return;
        }
        self.intake_hashinv = on;
        self.hashinv = on.then(|| rv_pdfauth_core::intake::locate(None));
        let found = match &self.hashinv {
            Some(Ok(t)) => {
                json!({"path": t.path.display().to_string(), "version": t.version, "sha256": t.sha256})
            }
            Some(Err(e)) => json!({"error": e}),
            None => Value::Null,
        };
        self.note(
            "ui_intake_changed",
            json!({"intake": if on { "hashinv" } else { "none" }, "hashinv": found}),
        );
    }

    /// The intake runs only with a case folder.
    pub fn intake_active(&self) -> bool {
        self.intake_hashinv && self.out_locked()
    }

    pub fn out_locked(&self) -> bool {
        !self.case_dir.trim().is_empty()
    }

    /// What the output-folder field shows: the derived run folder when a case folder is set.
    pub fn out_dir_display(&self) -> String {
        if self.out_locked() {
            PathBuf::from(self.case_dir.trim())
                .join("runs")
                .join(TOOL_NAME)
                .join("<RUN-ID>")
                .display()
                .to_string()
        } else {
            self.out_dir.clone()
        }
    }

    /// Result-panel lines: "E0001  acta.txt  — Verified: identical to the registered evidence".
    pub fn evidence_lines(&self) -> Vec<String> {
        let c = i18n::catalog(&self.ui_lang);
        self.evidence
            .iter()
            .map(|e| {
                let name = std::path::Path::new(&e.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| e.path.clone());
                format!(
                    "{}  {}  \u{2014} {}",
                    e.evidence_id.as_deref().unwrap_or("\u{2014}"),
                    name,
                    c.label(&format!("case_status_{}", e.status))
                )
            })
            .collect()
    }

    pub fn can_start(&self) -> bool {
        !self.is_running()
            && (!self.inputs.is_empty() || self.out_locked())
            && (self.out_locked() || !self.out_dir.trim().is_empty())
    }

    /// Builds the run configuration; the recorded UI actions become audit preamble entries.
    pub fn to_config(&self) -> Result<RunConfig, String> {
        if self.inputs.is_empty() && (!self.out_locked() || self.intake_active()) {
            return Err("Add at least one file or folder.".into());
        }
        let hashinv_exe = if self.intake_active() {
            match &self.hashinv {
                Some(Ok(t)) => Some(t.path.clone()),
                Some(Err(e)) => return Err(e.clone()),
                None => None,
            }
        } else {
            None
        };
        hash::validate_selection(&self.selected)?;
        let case_id = Some(self.case_id.trim().to_string()).filter(|s| !s.is_empty());
        // With the intake the case may not exist yet: rv-hashinv creates it and the core checks it afterwards.
        if self.out_locked() && !self.intake_active() {
            case::preflight(
                std::path::Path::new(self.case_dir.trim()),
                case_id.as_deref(),
                &[],
            )
            .map_err(|e| e.message)?;
        }
        let langs = i18n::parse_langs(&self.lang)?;
        if !self.sign_cert.is_empty() && !std::path::Path::new(&self.sign_cert).is_file() {
            return Err(format!("Signing certificate not found: {}", self.sign_cert));
        }
        if let Some(db) = &self.options.fingerprints_db
            && !db.is_file()
        {
            return Err(format!("Fingerprint database not found: {}", db.display()));
        }
        let mut preamble = self.ui_actions.clone();
        preamble.push(("ui_start_clicked".into(), json!({"algorithms": self.selected, "lang": self.lang, "recursive": self.recursive,
            "inputs": self.inputs.len(), "jurisdiction": self.jurisdiction, "timestamp": self.timestamp,
            "pdf_signing": !self.sign_cert.is_empty(), "options": self.options_json(),
            "case_dir": if self.out_locked() { json!(self.case_dir.trim()) } else { Value::Null },
            "at_utc": rv_pdfauth_core::time::utc_now()})));
        let settings = vec![
            format!("{} (gui)", TOOL_NAME),
            format!(
                "inputs={}",
                self.inputs
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(";")
            ),
            format!(
                "hash={}",
                self.selected
                    .iter()
                    .map(|a| a.name())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            format!("lang={}", self.lang),
            format!("recursive={}", self.recursive),
            format!("out={}", self.out_dir_display().trim()),
            format!(
                "case_dir={}",
                if self.out_locked() {
                    self.case_dir.trim()
                } else {
                    "none"
                }
            ),
            format!(
                "intake={}",
                if self.intake_active() {
                    "hashinv"
                } else {
                    "none"
                }
            ),
            format!("jurisdiction={}", self.jurisdiction),
            format!(
                "tsa={}",
                if self.timestamp {
                    self.tsa_url.as_str()
                } else {
                    "off"
                }
            ),
            format!(
                "sign_cert={}",
                if self.sign_cert.is_empty() {
                    "none"
                } else {
                    self.sign_cert.as_str()
                }
            ),
            format!("options={}", self.options_json()),
        ];
        Ok(RunConfig {
            inputs: self.inputs.clone(),
            input_list: None,
            recursive: self.recursive,
            algorithms: self.selected.clone(),
            out_dir: PathBuf::from(self.out_dir.trim()),
            langs,
            actor: if self.actor.trim().is_empty() {
                rv_pdfauth_core::env::os_user()
            } else {
                self.actor.trim().to_string()
            },
            case_id,
            frontend: "gui".into(),
            preamble,
            command_line: settings,
            tool_name: TOOL_NAME.into(),
            tool_version: TOOL_VERSION.into(),
            jurisdiction: self.jurisdiction.clone(),
            tsa_url: if self.timestamp {
                Some(self.tsa_url.clone())
            } else {
                None
            },
            sign_cert: if self.sign_cert.is_empty() {
                None
            } else {
                Some(PathBuf::from(&self.sign_cert))
            },
            sign_password: if self.sign_password.is_empty() {
                None
            } else {
                Some(self.sign_password.clone())
            },
            domain: self.domain_options(),
            case_dir: if self.out_locked() {
                Some(PathBuf::from(self.case_dir.trim()))
            } else {
                None
            },
            evidence_ids: vec![],
            case_role: CASE_ROLE,
            intake_hashinv: self.intake_active(),
            hashinv_exe,
        })
    }

    pub fn start(&mut self) {
        self.phase = Phase::Running;
        self.progress = Progress::default();
        self.current_item.clear();
        self.warnings.clear();
        self.outcome = None;
        self.results.clear();
        self.viewer = Viewer::default();
        self.evidence.clear();
        self.intake_progress = None;
        self.intake_report_dir = None;
        // Recorded actions now belong to this run's audit log.
        self.ui_actions.clear();
    }

    pub fn apply(&mut self, e: &Event) {
        match e {
            Event::RunStarted {
                total_items,
                total_bytes,
                ..
            } => {
                self.progress.total = *total_items;
                self.progress.bytes_total = *total_bytes;
            }
            Event::ItemStarted { path, .. } => self.current_item = path.clone(),
            Event::IntakeStarted { version, .. } => {
                self.current_item = format!("RV HashInv {version}");
                self.intake_progress = Some((0, 0));
            }
            Event::IntakeProgress { done, total, .. } => {
                self.intake_progress = Some((*done, *total))
            }
            Event::IntakeFinished { report_dir, .. } => self.intake_report_dir = report_dir.clone(),
            Event::EvidenceLinked {
                index,
                path,
                evidence_id,
                status,
            } => self.evidence.push(EvidenceLink {
                index: *index,
                path: path.clone(),
                evidence_id: evidence_id.clone(),
                status: status.clone(),
            }),
            Event::Progress {
                done,
                total,
                bytes_done,
                bytes_total,
                elapsed_ms,
                eta_ms,
            } => {
                self.progress = Progress {
                    done: *done,
                    total: *total,
                    bytes_done: *bytes_done,
                    bytes_total: *bytes_total,
                    elapsed_ms: *elapsed_ms,
                    eta_ms: *eta_ms,
                };
            }
            Event::Error { path, message } => {
                self.warnings
                    .push(format!("{}: {}", path.as_deref().unwrap_or("-"), message))
            }
            Event::RunFinished {
                items,
                errors,
                high,
                medium,
                low,
                info,
                report_dir,
                log_dir,
                cancelled,
                elapsed_ms,
                ..
            } => {
                self.progress.elapsed_ms = *elapsed_ms;
                self.progress.eta_ms = None;
                let lang = if self.lang == "all" {
                    "en".to_string()
                } else {
                    self.lang.clone()
                };
                self.outcome = Some(Outcome {
                    items: *items,
                    errors: *errors,
                    high: *high,
                    medium: *medium,
                    low: *low,
                    info: *info,
                    report: PathBuf::from(report_dir).join(format!("statistics.{}.html", lang)),
                    log_dir: PathBuf::from(log_dir),
                });
                self.phase = if *cancelled {
                    Phase::Cancelled
                } else {
                    Phase::Finished
                };
                self.current_item.clear();
            }
            _ => {}
        }
    }

    /// Fills the results table and the revision viewer from the core's summary.
    pub fn load_results(&mut self, summary: &RunSummary) {
        self.viewer = Viewer::from_results(&summary.results, &summary.report_dir);
        self.results = summary
            .results
            .iter()
            .map(|r| ResultRow {
                index: r.index,
                name: r.file_name(),
                status: r.status().to_string(),
                findings: r.findings.len(),
                top: r.findings.iter().map(|f| f.severity).max(),
                revisions: r.details["revisions"].as_array().map_or(0, Vec::len),
                evidence_id: r.evidence_id.clone(),
                viewer_item: self.viewer.item_for_index(r.index),
            })
            .collect();
    }

    /// "Review revisions" on a results row.
    pub fn open_viewer(&mut self, row: usize) {
        if let Some(i) = self.results.get(row).and_then(|r| r.viewer_item) {
            self.viewer.open_item(i);
        }
    }

    /// Findings of the viewer's item in the interface language, most severe first.
    pub fn viewer_findings(&self) -> Vec<FindingText> {
        let cat = i18n::catalog(&self.ui_lang);
        let mut out: Vec<FindingText> = self
            .viewer
            .current_item()
            .map(|it| it.findings.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|f| {
                let (title, shows, not_proves) = cat.finding(&f.code, &f.params);
                FindingText {
                    severity: f.severity,
                    code: f.code.clone(),
                    title,
                    shows,
                    not_proves,
                }
            })
            .collect();
        out.sort_by_key(|f| std::cmp::Reverse(f.severity));
        out
    }

    /// Viewer header: "#3  E0003  original.pdf" in a case folder ("#3  original.pdf" otherwise), and the
    /// registry check in the interface language ("Verified: identical to the registered evidence").
    pub fn viewer_header(&self) -> Option<(String, Option<String>)> {
        let it = self.viewer.current_item()?;
        let title = match &it.evidence_id {
            Some(id) => format!("#{}  {}  {}", it.index, id, it.name),
            None => format!("#{}  {}", it.index, it.name),
        };
        let check = it
            .case_status
            .as_ref()
            .map(|st| i18n::catalog(&self.ui_lang).label(&format!("case_status_{}", st)));
        Some((title, check))
    }

    pub fn severity_name(&self, s: Severity) -> String {
        i18n::catalog(&self.ui_lang).severity(s.key())
    }

    /// "+3 d 04 h" / "-2 h 05 min" (a revision dated before the previous one) / "—" (a date is missing).
    pub fn since_text(secs: Option<i64>) -> String {
        let Some(s) = secs else {
            return "\u{2014}".into();
        };
        let a = s.unsigned_abs();
        let body = match a {
            0..=59 => format!("{} s", a),
            60..=3599 => format!("{} min {:02} s", a / 60, a % 60),
            3600..=86_399 => format!("{} h {:02} min", a / 3600, (a % 3600) / 60),
            _ => format!("{} d {:02} h", a / 86_400, (a % 86_400) / 3600),
        };
        format!("{}{}", if s < 0 { "-" } else { "+" }, body)
    }

    pub fn fail(&mut self, msg: String) {
        self.phase = Phase::Failed(msg);
    }

    /// 0.0..=1.0, by bytes (or by files when every file is empty).
    pub fn fraction(&self) -> f32 {
        let p = &self.progress;
        if p.bytes_total > 0 {
            (p.bytes_done as f64 / p.bytes_total as f64).clamp(0.0, 1.0) as f32
        } else if p.total > 0 {
            (p.done as f64 / p.total as f64) as f32
        } else if matches!(self.phase, Phase::Finished) {
            1.0
        } else {
            0.0
        }
    }

    /// `live_elapsed_ms` is measured by the window clock so the timer keeps moving between events.
    pub fn timing_text(&self, live_elapsed_ms: Option<u64>) -> String {
        let elapsed = live_elapsed_ms
            .unwrap_or(self.progress.elapsed_ms)
            .max(self.progress.elapsed_ms);
        let eta = match self.progress.eta_ms {
            Some(ms) if self.is_running() => human_duration(ms),
            _ => "\u{2014}".into(),
        };
        format!(
            "{}: {}   \u{00B7}   {}: {}",
            self.tr("elapsed"),
            human_duration(elapsed),
            self.tr("remaining"),
            eta
        )
    }

    pub fn progress_text(&self) -> String {
        format!(
            "{} / {} {}  \u{00B7}  {:.0}%",
            self.progress.done,
            self.progress.total,
            self.tr("files"),
            self.fraction() * 100.0
        )
    }
}

// ---------------------------------------------------------------------------------------------------------
// Revision viewer: model built from `RunSummary::results` (ItemResult.details), no drawing code here.
// ---------------------------------------------------------------------------------------------------------

/// Zoom is relative to "fit the panel" (1.0).
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 16.0;
pub const ZOOM_STEP: f32 = 1.25;

/// Layout of `visual/rev-NN-vs-MM-page-PPP.png` as drawn by `rv_pdfauth_render::compose`: margin 12 px,
/// gutter 16 px, caption strip 56 px on top. Only used when the PNG size matches exactly (otherwise the
/// whole picture is shown as one image, with its burnt-in outlines).
const PNG_MARGIN: u32 = 12;
const PNG_GUTTER: u32 = 16;
const PNG_STRIP: u32 = 56;

#[derive(Clone, Debug, PartialEq)]
pub struct FindingText {
    pub severity: Severity,
    pub code: String,
    pub title: String,
    pub shows: String,
    pub not_proves: String,
}

/// One revision on the timeline strip.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RevView {
    pub index: usize,
    /// UTC date of the revision (Info ModDate, else CreationDate, else XMP ModifyDate).
    pub date: Option<String>,
    pub producer: Option<String>,
    /// Seconds since the previous revision's date (negative = dated before it); None if a date is missing.
    pub since_prev_s: Option<i64>,
    pub pages_changed: Vec<u32>,
    pub signed_by: Vec<String>,
    /// Findings attached to this revision (`revision` or `to_rev` parameter).
    pub badges: Vec<(Severity, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageView {
    pub page: u32,
    pub changed: bool,
    pub changed_ratio: String,
    /// Changed regions in page pixels: x, y, w, h.
    pub boxes: Vec<[u32; 4]>,
    pub width: u32,
    pub height: u32,
    /// Path of the diff picture relative to the item folder (only for changed pages).
    pub png: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PairView {
    pub from: usize,
    pub to: usize,
    pub status: String,
    pub reason: Option<String>,
    pub pages: Vec<PageView>,
    pub pages_added: Vec<u32>,
    pub pages_removed: Vec<u32>,
}

impl PageView {
    /// "0.0060%" from the core's ratio string "0.000060" (kept verbatim when it is not a number).
    pub fn changed_percent(&self) -> String {
        match self.changed_ratio.parse::<f64>() {
            Ok(r) => format!("{:.4}%", r * 100.0),
            Err(_) => self.changed_ratio.clone(),
        }
    }
}

impl PairView {
    pub fn has_changes(&self) -> bool {
        self.pages.iter().any(|p| p.changed)
            || !self.pages_added.is_empty()
            || !self.pages_removed.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewItem {
    pub index: u64,
    pub name: String,
    /// Case folder only: evidence id (E0001…) and how the item relates to the case registry
    /// (verified | mismatch | not_in_registry), shown in the viewer header.
    pub evidence_id: Option<String>,
    pub case_status: Option<String>,
    /// `<report dir>/items/<NNNNN-name>/`
    pub dir: PathBuf,
    pub findings: Vec<Finding>,
    pub revisions: Vec<RevView>,
    /// One per consecutive revision pair (r-1 vs r), from `details.visual.pairs` (or bare pairs when the
    /// visual diff did not run).
    pub pairs: Vec<PairView>,
    /// `details.visual.status`: done / skipped / not_applicable.
    pub visual_status: String,
    pub visual_reason: Option<String>,
    /// Audited digest of every output file: rel_path -> (algorithm, hex).
    pub digests: BTreeMap<String, (String, String)>,
}

/// Where the two halves sit in a diff PNG (pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Halves {
    pub left: [f32; 2],
    pub right: [f32; 2],
    pub size: [f32; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Viewer {
    pub open: bool,
    pub items: Vec<ViewItem>,
    pub item: usize,
    pub pair: usize,
    pub page: usize,
    pub zoom: f32,
    /// Offset from the centred position, in panel pixels (shared by both halves: synchronised pan).
    pub pan: [f32; 2],
}

impl Default for Viewer {
    fn default() -> Self {
        Viewer {
            open: false,
            items: vec![],
            item: 0,
            pair: 0,
            page: 0,
            zoom: 1.0,
            pan: [0.0, 0.0],
        }
    }
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(String::from)
}

fn u32s(v: &Value) -> Vec<u32> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_u64())
                .map(|x| x as u32)
                .collect()
        })
        .unwrap_or_default()
}

fn epoch(date: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(date)
        .ok()
        .map(|d| d.timestamp())
}

impl ViewItem {
    pub fn from_result(r: &ItemResult, report_dir: &Path) -> ViewItem {
        let d = &r.details;
        let mut revisions: Vec<RevView> = d["revisions"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, rv)| {
                let info = &rv["info"];
                let index = rv["index"].as_u64().map_or(i + 1, |x| x as usize);
                let tag = index.to_string();
                RevView {
                    index,
                    date: s(&info["mod_date"]["utc"])
                        .or_else(|| s(&info["creation_date"]["utc"]))
                        .or_else(|| s(&rv["xmp"]["modify_date"])),
                    producer: s(&info["producer"]).or_else(|| s(&rv["xmp"]["producer"])),
                    since_prev_s: None,
                    pages_changed: u32s(&rv["pages_changed"]),
                    signed_by: rv["signed_by"]
                        .as_array()
                        .map(|a| a.iter().filter_map(s).collect())
                        .unwrap_or_default(),
                    badges: r
                        .findings
                        .iter()
                        .filter(|f| {
                            f.params.get("revision") == Some(&tag)
                                || f.params.get("to_rev") == Some(&tag)
                        })
                        .map(|f| (f.severity, f.code.clone()))
                        .collect(),
                }
            })
            .collect();
        for i in 1..revisions.len() {
            let gap = revisions[i - 1]
                .date
                .as_deref()
                .and_then(epoch)
                .zip(revisions[i].date.as_deref().and_then(epoch))
                .map(|(a, b)| b - a);
            revisions[i].since_prev_s = gap;
        }
        let vis = &d["visual"];
        let mut pairs: Vec<PairView> = vis["pairs"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|p| PairView {
                from: p["from_rev"].as_u64().unwrap_or(0) as usize,
                to: p["to_rev"].as_u64().unwrap_or(0) as usize,
                status: s(&p["status"]).unwrap_or_default(),
                reason: s(&p["reason"]),
                pages_added: u32s(&p["pages_added"]),
                pages_removed: u32s(&p["pages_removed"]),
                pages: p["pages"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(|pg| PageView {
                        page: pg["page"].as_u64().unwrap_or(0) as u32,
                        changed: pg["changed"].as_bool().unwrap_or(false),
                        changed_ratio: s(&pg["changed_ratio"]).unwrap_or_default(),
                        boxes: pg["boxes"]
                            .as_array()
                            .map(Vec::as_slice)
                            .unwrap_or_default()
                            .iter()
                            .map(|b| {
                                let g = |k: &str| b[k].as_u64().unwrap_or(0) as u32;
                                [g("x"), g("y"), g("w"), g("h")]
                            })
                            .collect(),
                        width: pg["width"].as_u64().unwrap_or(0) as u32,
                        height: pg["height"].as_u64().unwrap_or(0) as u32,
                        png: s(&pg["png"]),
                    })
                    .collect(),
            })
            .collect();
        if pairs.is_empty() && revisions.len() > 1 {
            pairs = revisions
                .windows(2)
                .map(|w| PairView {
                    from: w[0].index,
                    to: w[1].index,
                    status: s(&vis["status"]).unwrap_or_else(|| "not_run".into()),
                    reason: s(&vis["reason"]),
                    ..Default::default()
                })
                .collect();
        }
        ViewItem {
            index: r.index,
            name: r.file_name(),
            evidence_id: r.evidence_id.clone(),
            case_status: r.case_status.clone(),
            dir: report_dir.join("items").join(r.folder()),
            findings: r.findings.clone(),
            revisions,
            pairs,
            visual_status: s(&vis["status"]).unwrap_or_else(|| "not_run".into()),
            visual_reason: s(&vis["reason"]),
            digests: r
                .outputs
                .iter()
                .filter_map(|o| {
                    o.hashes
                        .iter()
                        .next()
                        .map(|(a, h)| (o.rel_path.clone(), (a.clone(), h.clone())))
                })
                .collect(),
        }
    }
}

impl Viewer {
    /// Items with two or more revisions (something to compare), in run order.
    pub fn from_results(results: &[ItemResult], report_dir: &Path) -> Viewer {
        Viewer {
            items: results
                .iter()
                .map(|r| ViewItem::from_result(r, report_dir))
                .filter(|v| v.revisions.len() > 1 || !v.pairs.is_empty())
                .collect(),
            ..Default::default()
        }
    }

    pub fn item_for_index(&self, index: u64) -> Option<usize> {
        self.items.iter().position(|v| v.index == index)
    }

    pub fn current_item(&self) -> Option<&ViewItem> {
        self.items.get(self.item)
    }

    pub fn current_pair(&self) -> Option<&PairView> {
        self.current_item().and_then(|i| i.pairs.get(self.pair))
    }

    pub fn current_page(&self) -> Option<&PageView> {
        self.current_pair().and_then(|p| p.pages.get(self.page))
    }

    pub fn open_item(&mut self, i: usize) {
        if i < self.items.len() {
            self.open = true;
            self.select_item(i);
        }
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Selects an item and jumps to its first pair with a change (else the first pair).
    pub fn select_item(&mut self, i: usize) {
        self.item = i.min(self.items.len().saturating_sub(1));
        let first = self
            .current_item()
            .and_then(|it| it.pairs.iter().position(PairView::has_changes))
            .unwrap_or(0);
        self.select_pair(first);
    }

    /// Selects a revision pair and jumps to its first changed page (else the first page).
    pub fn select_pair(&mut self, p: usize) {
        let n = self.current_item().map_or(0, |i| i.pairs.len());
        self.pair = p.min(n.saturating_sub(1));
        let first = self
            .current_pair()
            .and_then(|pr| pr.pages.iter().position(|pg| pg.changed))
            .unwrap_or(0);
        self.select_page(first);
    }

    pub fn select_page(&mut self, p: usize) {
        let n = self.current_pair().map_or(0, |pr| pr.pages.len());
        self.page = p.min(n.saturating_sub(1));
        self.reset_view();
    }

    pub fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan = [0.0, 0.0];
    }

    /// The revisions of the selected pair (highlighted on the timeline).
    pub fn pair_revisions(&self) -> Option<(usize, usize)> {
        self.current_pair().map(|p| (p.from, p.to))
    }

    pub fn png_path(&self) -> Option<PathBuf> {
        let it = self.current_item()?;
        self.current_page()?.png.as_ref().map(|r| it.dir.join(r))
    }

    /// The digest recorded in the audit log for the selected PNG: (algorithm, hex).
    pub fn png_digest(&self) -> Option<(String, String)> {
        let rel = self.current_page()?.png.as_ref()?;
        self.current_item()?.digests.get(rel).cloned()
    }

    pub fn set_zoom(&mut self, z: f32) {
        self.zoom = if z.is_finite() {
            z.clamp(ZOOM_MIN, ZOOM_MAX)
        } else {
            1.0
        };
    }

    pub fn pan_by(&mut self, d: [f32; 2]) {
        self.pan[0] += d[0];
        self.pan[1] += d[1];
    }

    /// Scale and top-left offset (panel pixels) of an image of `img` pixels shown in a `panel`.
    pub fn transform(&self, panel: [f32; 2], img: [f32; 2]) -> (f32, [f32; 2]) {
        if img[0] <= 0.0 || img[1] <= 0.0 {
            return (1.0, [0.0, 0.0]);
        }
        let fit = (panel[0] / img[0]).min(panel[1] / img[1]).max(1e-6);
        let sc = fit * self.zoom;
        (
            sc,
            [
                (panel[0] - img[0] * sc) / 2.0 + self.pan[0],
                (panel[1] - img[1] * sc) / 2.0 + self.pan[1],
            ],
        )
    }

    /// Zooms by `factor` keeping the image point under `anchor` (panel pixels) in place.
    pub fn zoom_at(&mut self, factor: f32, anchor: [f32; 2], panel: [f32; 2], img: [f32; 2]) {
        let (s0, o0) = self.transform(panel, img);
        let u = [(anchor[0] - o0[0]) / s0, (anchor[1] - o0[1]) / s0];
        self.set_zoom(self.zoom * factor);
        let (s1, o1) = self.transform(panel, img);
        self.pan[0] += anchor[0] - (o1[0] + u[0] * s1);
        self.pan[1] += anchor[1] - (o1[1] + u[1] * s1);
    }

    /// Splits a diff PNG into its "previous" and "current" halves when its size matches the compose layout
    /// for this page; None = show the PNG as one picture.
    pub fn halves(png: [u32; 2], page: [u32; 2]) -> Option<Halves> {
        let [pw, ph] = page;
        if pw == 0
            || ph == 0
            || png[0] != 2 * PNG_MARGIN + PNG_GUTTER + 2 * pw
            || png[1] != PNG_STRIP + ph + PNG_MARGIN
        {
            return None;
        }
        Some(Halves {
            left: [PNG_MARGIN as f32, PNG_STRIP as f32],
            right: [(PNG_MARGIN + pw + PNG_GUTTER) as f32, PNG_STRIP as f32],
            size: [pw as f32, ph as f32],
        })
    }
}

/// Checks a file's bytes against the digest recorded in the audit log. None = no digest / unknown algorithm.
pub fn verify_digest(bytes: &[u8], expected: &(String, String)) -> Option<bool> {
    let algo = HashAlgo::parse(&expected.0)?;
    Some(hash::hash_bytes(bytes, algo).eq_ignore_ascii_case(&expected.1))
}

/// Decodes a PNG into RGBA8 (width, height, pixels) for display.
pub fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "image too large".to_string())?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let px = (w as usize) * (h as usize);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Indexed => return Err("indexed PNG not expanded".into()),
    };
    if rgba.len() != px * 4 {
        return Err("unexpected PNG buffer size".into());
    }
    Ok((w, h, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_most_two_and_at_least_one_algorithm() {
        let mut s = GuiState::default();
        assert_eq!(s.selected, vec![HashAlgo::Sha256]);
        assert!(
            !s.can_toggle(HashAlgo::Sha256),
            "cannot untick the last one"
        );
        assert!(s.toggle_algo(HashAlgo::Sha256).is_err());
        s.toggle_algo(HashAlgo::Md5).unwrap();
        assert!(
            !s.can_toggle(HashAlgo::Blake3),
            "third checkbox is disabled"
        );
        assert!(s.toggle_algo(HashAlgo::Blake3).is_err());
        assert!(s.can_toggle(HashAlgo::Sha256));
        s.toggle_algo(HashAlgo::Sha256).unwrap();
        assert_eq!(s.selected, vec![HashAlgo::Md5]);
    }

    #[test]
    fn settings_changes_become_audit_preamble() {
        let mut s = GuiState::default();
        assert!(s.to_config().is_err(), "no input yet");
        s.path_entry = "\"C:\\evidence\"".into();
        s.add_typed_path();
        s.toggle_algo(HashAlgo::Blake3).unwrap();
        s.set_lang("es");
        let cfg = s.to_config().unwrap();
        assert_eq!(cfg.inputs, vec![PathBuf::from("C:\\evidence")]);
        assert_eq!(cfg.frontend, "gui");
        let actions: Vec<&str> = cfg.preamble.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(
            actions,
            vec![
                "ui_input_added",
                "ui_hash_selection_changed",
                "ui_language_changed",
                "ui_start_clicked"
            ]
        );
        s.start();
        assert!(s.ui_actions.is_empty());
    }

    #[test]
    fn events_drive_progress_and_outcome() {
        let mut s = GuiState::default();
        s.start();
        s.apply(&Event::RunStarted {
            run_id: "R".into(),
            tool: "t".into(),
            version: "v".into(),
            frontend: "gui".into(),
            algorithms: vec![HashAlgo::Sha256],
            langs: vec!["en".into()],
            total_items: 4,
            total_bytes: 400,
        });
        s.apply(&Event::Progress {
            done: 1,
            total: 4,
            bytes_done: 100,
            bytes_total: 400,
            elapsed_ms: 2000,
            eta_ms: Some(6000),
        });
        assert!((s.fraction() - 0.25).abs() < 1e-6);
        assert!(s.timing_text(Some(2500)).contains("6.0 s"));
        assert!(s.progress_text().starts_with("1 / 4"));
        s.apply(&Event::RunFinished {
            items: 4,
            bytes: 400,
            errors: 0,
            elapsed_ms: 8000,
            high: 1,
            medium: 0,
            low: 0,
            info: 0,
            report_dir: "out/reports/R".into(),
            log_dir: "out/logs/R".into(),
            cancelled: false,
        });
        assert_eq!(s.phase, Phase::Finished);
        assert_eq!(
            s.outcome.as_ref().unwrap().report,
            PathBuf::from("out/reports/R/statistics.en.html")
        );
    }

    #[test]
    fn interface_language_and_trust_settings_are_audited() {
        let mut s = GuiState::default();
        s.set_ui_lang("fr");
        assert_eq!(s.tr("start"), "Lancer l'analyse");
        s.set_ui_lang("es");
        assert_eq!(s.tr("start"), "Iniciar revisión");
        s.set_jurisdiction("fr");
        s.set_timestamp(false, "http://tsa.example");
        s.set_sign_cert("missing.p12");
        s.add_input(PathBuf::from("."));
        assert!(s.to_config().is_err(), "missing certificate refused");
        s.set_sign_cert("");
        let cfg = s.to_config().unwrap();
        assert_eq!(cfg.jurisdiction, "fr");
        assert!(cfg.tsa_url.is_none());
        let actions: Vec<&str> = cfg.preamble.iter().map(|(a, _)| a.as_str()).collect();
        assert!(
            actions.contains(&"ui_interface_language_changed")
                && actions.contains(&"ui_timestamp_changed")
        );
        assert!(
            cfg.preamble
                .iter()
                .all(|(_, d)| !d.to_string().contains("password"))
        );
    }

    #[test]
    fn case_folder_locks_the_output_folder_and_shows_evidence_ids() {
        let tmp = std::env::temp_dir().join(format!("rv-gui-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("ev")).unwrap();
        std::fs::write(tmp.join("ev/a.txt"), "a").unwrap();
        std::fs::write(tmp.join("ev/b.txt"), "b").unwrap();
        let mut s = GuiState {
            timestamp: false,
            ..Default::default()
        };
        assert!(!s.can_start());
        let case_dir = tmp.join("CASE-G");
        s.set_case_dir(&format!("\"{}\"", case_dir.display()));
        assert!(
            s.out_locked() && s.can_start(),
            "a case folder alone can start (registered items)"
        );
        assert!(s.out_dir_display().contains("runs") && s.out_dir_display().ends_with("<RUN-ID>"));
        assert!(s.to_config().is_err(), "creating a case needs a case id");
        s.case_id = "CASE-G".into();
        s.add_input(tmp.join("ev"));
        if CASE_ROLE == rv_pdfauth_core::CaseRole::Analysis {
            // An analysis tool verifies against a registry: create it with an explicit intake run first.
            let seed = RunConfig {
                inputs: vec![tmp.join("ev")],
                case_id: Some("CASE-G".into()),
                case_dir: Some(case_dir.clone()),
                case_role: rv_pdfauth_core::CaseRole::Intake,
                ..Default::default()
            };
            let off = std::sync::atomic::AtomicBool::new(false);
            rv_pdfauth_core::run(&seed, &mut |_| {}, &off).unwrap();
        }
        let cfg = s.to_config().unwrap();
        assert_eq!(cfg.case_dir.as_deref(), Some(case_dir.as_path()));
        assert_eq!(cfg.case_role, CASE_ROLE);
        let actions: Vec<&str> = cfg.preamble.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(actions[0], "ui_case_dir_changed");
        s.start();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let summary = rv_pdfauth_core::run(&cfg, &mut |e| s.apply(e), &cancel).unwrap();
        assert_eq!(s.phase, Phase::Finished);
        assert!(summary.log_dir.starts_with(&case_dir));
        let ids: Vec<Option<&str>> = s
            .evidence
            .iter()
            .map(|e| e.evidence_id.as_deref())
            .collect();
        assert_eq!(ids, vec![Some("E0001"), Some("E0002")]);
        assert!(s.evidence_lines()[0].starts_with("E0001  a.txt"));
        assert!(s.evidence_lines()[0].contains("Verified"));
        s.load_results(&summary);
        assert_eq!(s.results[1].evidence_id.as_deref(), Some("E0002"));
        let log = std::fs::read_to_string(summary.log_dir.join("audit.jsonl")).unwrap();
        assert!(log.contains("\"ui_case_dir_changed\"") && log.contains("\"case_linked\""));
        s.set_case_dir("");
        assert!(!s.out_locked() && s.out_dir_display() == "rakvor-out");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_real_run_from_gui_settings_is_audited() {
        let ev = std::env::temp_dir().join(format!("rv-gui-test-{}", std::process::id()));
        std::fs::create_dir_all(&ev).unwrap();
        std::fs::write(ev.join("a.txt"), "x").unwrap();
        let mut s = GuiState {
            timestamp: false, // tests stay offline
            ..Default::default()
        };
        s.add_input(ev.clone());
        s.out_dir = ev.join("out").display().to_string();
        let cfg = s.to_config().unwrap();
        s.start();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let summary = rv_pdfauth_core::run(&cfg, &mut |e| s.apply(e), &cancel).unwrap();
        assert_eq!(s.phase, Phase::Finished);
        let log = std::fs::read_to_string(summary.log_dir.join("audit.jsonl")).unwrap();
        assert!(
            log.contains("\"ui_input_added\"")
                && log.contains("\"ui_start_clicked\"")
                && log.contains("\"frontend\":\"gui\"")
        );
        let _ = std::fs::remove_dir_all(&ev);
    }

    #[test]
    fn options_map_one_to_one_onto_domain_options_and_are_audited() {
        let mut s = GuiState::default();
        assert_eq!(s.options, DomainOptions::default());
        let changes = [
            OptionChange::ExtractRevisions(false),
            OptionChange::VisualDiff(false),
            OptionChange::Dpi(150),
            OptionChange::MaxPages(7),
            OptionChange::ObjectsDump(false),
            OptionChange::ExtractEmbedded(false),
            OptionChange::WorkerTimeout(300),
            OptionChange::WorkerMemory(4096),
            OptionChange::ItemReportsFlaggedOnly(true),
        ];
        for c in changes.iter().cloned() {
            assert!(s.set_option(c));
        }
        // Same value again: no change, no audit entry.
        assert!(!s.set_option(OptionChange::Dpi(150)));
        // Out-of-range numbers are clamped.
        assert!(s.set_option(OptionChange::Dpi(5000)));
        assert_eq!(s.options.dpi, *DPI_RANGE.end());
        s.set_option(OptionChange::MaxPages(0));
        assert_eq!(s.options.max_pages, 1);
        s.set_option(OptionChange::WorkerTimeout(1));
        assert_eq!(s.options.worker_timeout_s, *TIMEOUT_RANGE.start());
        s.set_option(OptionChange::WorkerMemory(1));
        assert_eq!(s.options.worker_memory_mb, *MEMORY_RANGE.start());

        let d = s.domain_options();
        assert!(!d.extract_revisions && !d.visual_diff && !d.objects_dump && !d.extract_embedded);
        assert!(d.item_reports_flagged_only);
        assert!(matches!(d.isolation, Isolation::Subprocess { .. }));

        let names: Vec<&str> = s.ui_actions.iter().map(|(a, _)| a.as_str()).collect();
        for c in &changes {
            assert!(
                names.contains(&c.audit_name()),
                "{} audited",
                c.audit_name()
            );
        }
        assert!(s.ui_actions.iter().all(|(_, d)| d["at_utc"].is_string()));
        let flagged = s
            .ui_actions
            .iter()
            .find(|(a, _)| a == "ui_item_reports_changed")
            .unwrap();
        assert_eq!(flagged.1["value"], "flagged");

        // Fingerprint DB: missing file refused at Start, existing file accepted, cleared = None.
        s.add_input(PathBuf::from("."));
        s.set_option(OptionChange::FingerprintsDb("\"nope-fp.json\"".into()));
        assert_eq!(s.fp_db_entry, "nope-fp.json");
        assert!(s.to_config().is_err());
        let db = std::env::temp_dir().join(format!("rv-gui-fp-{}.json", std::process::id()));
        std::fs::write(&db, "{}").unwrap();
        s.set_option(OptionChange::FingerprintsDb(db.display().to_string()));
        let cfg = s.to_config().unwrap();
        assert_eq!(cfg.domain.fingerprints_db.as_deref(), Some(db.as_path()));
        assert_eq!(cfg.domain.dpi, *DPI_RANGE.end());
        let start = &cfg.preamble.last().unwrap().1;
        assert_eq!(start["options"]["item_reports"], "flagged");
        assert!(cfg.command_line.iter().any(|l| l.starts_with("options=")));
        s.set_option(OptionChange::FingerprintsDb(String::new()));
        assert!(s.options.fingerprints_db.is_none());
        let _ = std::fs::remove_file(&db);
    }

    fn page(n: u32, changed: bool) -> PageView {
        PageView {
            page: n,
            changed,
            changed_ratio: if changed { "0.001".into() } else { "0".into() },
            boxes: if changed {
                vec![[10, 20, 30, 40]]
            } else {
                vec![]
            },
            width: 935,
            height: 1210,
            png: changed.then(|| format!("visual/rev-01-vs-02-page-{:03}.png", n)),
        }
    }

    fn sample_viewer() -> Viewer {
        let pair = |from, to, pages: Vec<PageView>| PairView {
            from,
            to,
            status: "done".into(),
            pages,
            ..Default::default()
        };
        Viewer {
            items: vec![
                ViewItem {
                    index: 1,
                    name: "a.pdf".into(),
                    pairs: vec![pair(1, 2, vec![page(1, false), page(2, false)])],
                    ..Default::default()
                },
                ViewItem {
                    index: 4,
                    name: "b.pdf".into(),
                    pairs: vec![
                        pair(1, 2, vec![page(1, false)]),
                        pair(2, 3, vec![page(1, false), page(2, true), page(3, true)]),
                    ],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn viewer_header_shows_the_evidence_id_in_a_case_folder() {
        let mut s = GuiState {
            viewer: sample_viewer(),
            ..Default::default()
        };
        s.viewer.open_item(0);
        assert_eq!(s.viewer_header().unwrap(), ("#1  a.pdf".to_string(), None));
        s.viewer.items[1].evidence_id = Some("E0007".into());
        s.viewer.items[1].case_status = Some("mismatch".into());
        s.viewer.open_item(1);
        let (title, check) = s.viewer_header().unwrap();
        assert_eq!(title, "#4  E0007  b.pdf");
        assert_eq!(
            check.as_deref(),
            Some("MISMATCH with the registered evidence")
        );
        s.ui_lang = "es".into();
        assert_eq!(
            s.viewer_header().unwrap().1.as_deref(),
            Some("NO COINCIDE con la evidencia registrada")
        );
    }

    #[test]
    fn viewer_selection_jumps_to_changes_and_clamps() {
        let mut v = sample_viewer();
        assert_eq!(v.item_for_index(4), Some(1));
        assert_eq!(v.item_for_index(2), None);
        v.open_item(1);
        assert!(v.open);
        assert_eq!(
            (v.pair, v.page),
            (1, 1),
            "first changed pair, first changed page"
        );
        assert_eq!(v.pair_revisions(), Some((2, 3)));
        assert_eq!(v.current_page().unwrap().page, 2);
        v.select_page(99);
        assert_eq!(v.page, 2, "page clamped to the last one");
        v.select_pair(0);
        assert_eq!((v.pair, v.page), (0, 0), "no change: first page");
        assert!(v.png_path().is_none(), "unchanged page has no picture");
        v.select_pair(5);
        assert_eq!(v.pair, 1);
        v.select_item(0);
        assert_eq!((v.item, v.pair, v.page), (0, 0, 0));
        v.open_item(9);
        assert_eq!(v.item, 0, "out-of-range item ignored");
        v.close();
        assert!(!v.open);
        // Empty viewer never panics.
        let mut e = Viewer::default();
        e.select_item(3);
        e.select_pair(2);
        e.select_page(1);
        assert!(e.current_item().is_none() && e.png_path().is_none());
    }

    #[test]
    fn zoom_is_clamped_and_pan_is_shared() {
        let mut v = sample_viewer();
        v.open_item(1);
        v.set_zoom(100.0);
        assert_eq!(v.zoom, ZOOM_MAX);
        v.set_zoom(0.01);
        assert_eq!(v.zoom, ZOOM_MIN);
        v.set_zoom(f32::NAN);
        assert_eq!(v.zoom, 1.0);
        // Fit: a 1000x500 image in a 500x500 panel is scaled by 0.5 and centred vertically.
        let (sc, off) = v.transform([500.0, 500.0], [1000.0, 500.0]);
        assert!((sc - 0.5).abs() < 1e-6);
        assert!((off[0]).abs() < 1e-3 && (off[1] - 125.0).abs() < 1e-3);
        // Zooming at a point keeps that image point under the cursor.
        let anchor = [100.0, 200.0];
        let before = v.transform([500.0, 500.0], [1000.0, 500.0]);
        let u = [
            (anchor[0] - before.1[0]) / before.0,
            (anchor[1] - before.1[1]) / before.0,
        ];
        v.zoom_at(2.0, anchor, [500.0, 500.0], [1000.0, 500.0]);
        assert!((v.zoom - 2.0).abs() < 1e-6);
        let (s1, o1) = v.transform([500.0, 500.0], [1000.0, 500.0]);
        assert!((o1[0] + u[0] * s1 - anchor[0]).abs() < 1e-2);
        assert!((o1[1] + u[1] * s1 - anchor[1]).abs() < 1e-2);
        v.zoom_at(1000.0, anchor, [500.0, 500.0], [1000.0, 500.0]);
        assert_eq!(v.zoom, ZOOM_MAX);
        v.pan_by([5.0, -3.0]);
        let p = v.pan;
        v.select_page(2);
        assert_eq!(
            (v.zoom, v.pan),
            (1.0, [0.0, 0.0]),
            "new page resets the view"
        );
        assert_ne!(p, [0.0, 0.0]);
        // PNG split follows the compose layout exactly, else None.
        let h = Viewer::halves([1910, 1278], [935, 1210]).unwrap();
        assert_eq!(h.left, [12.0, 56.0]);
        assert_eq!(h.right, [963.0, 56.0]);
        assert!(Viewer::halves([1911, 1278], [935, 1210]).is_none());
        assert!(Viewer::halves([1910, 1278], [0, 0]).is_none());
    }

    #[test]
    fn timeline_gaps_and_digests() {
        assert_eq!(GuiState::since_text(None), "\u{2014}");
        assert_eq!(GuiState::since_text(Some(42)), "+42 s");
        assert_eq!(
            GuiState::since_text(Some(3 * 86_400 + 4 * 3600)),
            "+3 d 04 h"
        );
        assert_eq!(GuiState::since_text(Some(-7500)), "-2 h 05 min");
        let exp = (
            "sha256".to_string(),
            hash::hash_bytes(b"abc", HashAlgo::Sha256),
        );
        let mut p = page(1, true);
        p.changed_ratio = "0.000060".into();
        assert_eq!(p.changed_percent(), "0.0060%");
        p.changed_ratio = "n/a".into();
        assert_eq!(p.changed_percent(), "n/a");
        assert_eq!(verify_digest(b"abc", &exp), Some(true));
        assert_eq!(verify_digest(b"abd", &exp), Some(false));
        assert_eq!(verify_digest(b"abc", &("crc".into(), "0".into())), None);
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn ui_catalogues_have_identical_keys() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../i18n");
        let keys = |l: &str| -> Vec<String> {
            let v: Value = serde_json::from_str(
                &std::fs::read_to_string(root.join(format!("{l}.json"))).unwrap(),
            )
            .unwrap();
            let mut k: Vec<String> = v["ui"].as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        let en = keys("en");
        assert_eq!(en, keys("es"));
        assert_eq!(en, keys("fr"));
        for k in [
            "options",
            "opt_dpi",
            "review",
            "viewer_title",
            "timeline",
            "shows",
            "not_proves",
        ] {
            assert!(en.iter().any(|x| x == k), "missing ui key {k}");
        }
        // Every value is a non-empty string, and the embedded catalogue serves it (tr() would fall back to
        // the key itself).
        for l in ["en", "es", "fr"] {
            let v: Value = serde_json::from_str(
                &std::fs::read_to_string(root.join(format!("{l}.json"))).unwrap(),
            )
            .unwrap();
            let s = GuiState {
                ui_lang: l.into(),
                ..Default::default()
            };
            for k in &en {
                let want = v["ui"][k].as_str().unwrap_or_default();
                assert!(!want.trim().is_empty(), "{l}: {k} empty");
                assert_eq!(s.tr(k), want, "{l}: {k}");
            }
        }
    }

    #[test]
    fn sample_run_loads_the_revision_viewer() {
        let ev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sample/evidence");
        let out = std::env::temp_dir().join(format!("rv-gui-sample-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let mut s = GuiState {
            timestamp: false, // tests stay offline
            ..Default::default()
        };
        s.add_input(ev);
        s.out_dir = out.display().to_string();
        let mut cfg = s.to_config().unwrap();
        // The test binary is not the GUI exe (no `worker` entry point): analyse in process here.
        cfg.domain.isolation = Isolation::InProcess;
        s.start();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let summary = rv_pdfauth_core::run(&cfg, &mut |e| s.apply(e), &cancel).unwrap();
        assert_eq!(s.phase, Phase::Finished);
        s.load_results(&summary);
        assert_eq!(s.results.len(), summary.results.len());
        assert!(s.results.len() >= 7);
        let row = s
            .results
            .iter()
            .position(|r| r.name == "edited-incremental.pdf")
            .unwrap();
        assert!(s.results[row].revisions >= 3);
        assert!(s.results[row].viewer_item.is_some());
        assert!(
            s.results
                .iter()
                .filter(|r| r.revisions < 2)
                .all(|r| r.viewer_item.is_none())
        );
        s.open_viewer(row);
        let v = &s.viewer;
        assert!(v.open);
        let it = v.current_item().unwrap();
        assert_eq!(it.name, "edited-incremental.pdf");
        assert!(it.revisions.len() >= 3);
        assert!(it.revisions.iter().any(|r| r.producer.is_some()));
        assert!(it.revisions.iter().any(|r| !r.badges.is_empty()));
        assert_eq!(it.pairs.len(), it.revisions.len() - 1);
        let f = s.viewer_findings();
        assert!(!f.is_empty() && f.windows(2).all(|w| w[0].severity >= w[1].severity));
        assert!(
            f.iter()
                .all(|x| !x.shows.is_empty() && !x.not_proves.is_empty())
        );
        if it.visual_status == "done" {
            let (from, to) = v.pair_revisions().unwrap();
            assert_eq!((from, to), (1, 2), "first pair with a change: the amount");
            let pg = v.current_page().unwrap();
            assert!(pg.changed && !pg.boxes.is_empty());
            let png = v.png_path().unwrap();
            let bytes = std::fs::read(&png).unwrap();
            assert_eq!(verify_digest(&bytes, &v.png_digest().unwrap()), Some(true));
            let (w, h, rgba) = decode_png(&bytes).unwrap();
            assert_eq!(rgba.len(), (w * h * 4) as usize);
            assert!(Viewer::halves([w, h], [pg.width, pg.height]).is_some());
        } else {
            assert!(
                std::env::var_os("RV_PDFIUM_PATH").is_none(),
                "PDFium available but visual diff {}",
                it.visual_status
            );
        }
        let _ = std::fs::remove_dir_all(&out);
    }
}
