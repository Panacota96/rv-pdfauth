# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Entries are generated from
[Conventional Commits](https://www.conventionalcommits.org/) by release-please and reviewed by a human.

## [Unreleased]

### Added
- Project scaffolded with rakvor-tool-forge (rust profile) on 2026-10-21.
- Shared case folder (`--case-dir DIR`, `--evidence E0001,…`), analysis role: every item is re-hashed and verified
  against the evidence registry written by the intake tool (rv-hashinv) before the PDF analysis
  (`EVIDENCE_HASH_MISMATCH_WITH_CASE` high, `EVIDENCE_NOT_IN_CASE_REGISTRY` medium, `case_algorithm_added`);
  outputs go to `<CASE>/runs/rv-pdfauth/<RUN-ID>/`; `run_started` / `run_finished` are recorded in the hash-chained
  `case-log.jsonl`. Registry paths are matched case-insensitively on Windows.
- `rv-pdfauth verify-case <CASE>`: case log, registry digest, every run's audit chain and the cross-links (exit 0/3).
- Evidence ids in every report (technical, summary, statistics with a "Case folder" section, JSON), custody forms
  with the case id, evidence id and intake date; GUI case-folder field, evidence ids in the results table and the
  revision viewer header.
