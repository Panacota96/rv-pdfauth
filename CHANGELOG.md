# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Entries are generated from
[Conventional Commits](https://www.conventionalcommits.org/) by release-please and reviewed by a human.

## [0.4.0](https://github.com/Panacota96/rv-pdfauth/compare/v0.3.0...v0.4.0) (2026-10-29)


### Features

* **cli:** describe --json prints the tool's self-description for RV Hub ([77a052a](https://github.com/Panacota96/rv-pdfauth/commit/77a052aa17ba69300ed280a322b34fcd86ea5f78))
* **gui:** --case-dir DIR presets the case folder when RV Hub launches the app ([a78bc40](https://github.com/Panacota96/rv-pdfauth/commit/a78bc405bf8b3fa8e3bd26aec107b088f96366e8))

## [0.3.0](https://github.com/Panacota96/rv-pdfauth/compare/v0.2.1...v0.3.0) (2026-10-27)


### Features

* **case:** one-step intake with RV HashInv (--intake hashinv) ([e18140e](https://github.com/Panacota96/rv-pdfauth/commit/e18140e35484fdebb1602aa2805a8528b4c406c8))

## [0.2.1](https://github.com/Panacota96/rv-pdfauth/compare/v0.2.0...v0.2.1) (2026-10-27)


### Bug Fixes

* **windows:** Rakvor icon on the executables, shortcuts and window title bar ([e7f7667](https://github.com/Panacota96/rv-pdfauth/commit/e7f76679c90e7ca4594f1a6c401b59439636e8f1))

## [0.2.0](https://github.com/Panacota96/rv-pdfauth/compare/v0.1.0...v0.2.0) (2026-10-22)


### Features

* **case:** shared case folder, analysis role (--case-dir, --evidence, verify-case) ([83a4443](https://github.com/Panacota96/rv-pdfauth/commit/83a4443f9c9368de5869d4d132ba914a72697086))
* **cli:** analyze command, PDF domain options, fingerprints DB and PDF report sections ([2db243e](https://github.com/Panacota96/rv-pdfauth/commit/2db243e7f9ae3c0ff886f68702ed6bd894617fde))
* **core:** re-hash the parsed evidence in the worker (EVIDENCE_CHANGED_DURING_ANALYSIS) ([444ba8c](https://github.com/Panacota96/rv-pdfauth/commit/444ba8c54378f748ec634102b51bab227b59b355))
* **core:** return per-item results in RunSummary ([a0c2143](https://github.com/Panacota96/rv-pdfauth/commit/a0c21439dc07f818b6df96ea59023bcbcf9fb622))
* **custody:** position, ID type/number, witnesses and assistants in the custody form ([e47b26d](https://github.com/Panacota96/rv-pdfauth/commit/e47b26d02cfba7de94e6d12e2a24fad45121ff93))
* freeze PDF data model and domain analysis interfaces ([39bf143](https://github.com/Panacota96/rv-pdfauth/commit/39bf14374a8c8864b581b06ff00df6a5e38fe195))
* **gui:** analysis options section and revision viewer ([2e842fc](https://github.com/Panacota96/rv-pdfauth/commit/2e842fca7d5f8ae2c7b2013f5976b5fdb64f1392))
* PDF analysis with worker isolation, PDFium rendering and visual diff ([3ec850c](https://github.com/Panacota96/rv-pdfauth/commit/3ec850c757d71c8aebab5b2ceaeed568b90f037c))
* **pdf:** per-revision page count and pikepdf/pyHanko oracle script ([90f5f4b](https://github.com/Panacota96/rv-pdfauth/commit/90f5f4bb55e80428fe1811440f86551f7416eab4))
* **pdf:** raw PDF analyzer with revisions, xref, metadata, signatures and fingerprints ([0c6f48d](https://github.com/Panacota96/rv-pdfauth/commit/0c6f48d6d5022ee363e49a636b3978d352d1b626))
* **pdf:** REVISION_DATE_NOT_MONOTONIC finding; worded VISUAL_DIFF_UNAVAILABLE limitation ([7f7e83d](https://github.com/Panacota96/rv-pdfauth/commit/7f7e83dbdf60dda5f0ca851561b37d34cc096d88))
* **report:** persist parser warnings and per-page visual-diff data ([e09f543](https://github.com/Panacota96/rv-pdfauth/commit/e09f543a49ec9f6a70af0c5c5119daef92f00b8f))
* **trust:** verify the TSA's CMS signature and, with --tsa-ca, its certificate chain ([3c40045](https://github.com/Panacota96/rv-pdfauth/commit/3c400456e2a4f494dc3e1f9388ed179f34b9007f))
* wire the PDF layer into the core after merging main ([1fead1f](https://github.com/Panacota96/rv-pdfauth/commit/1fead1f08df3bb3f525a2c47cbf93e1f6798355a))
* **worker:** network namespace on Linux; record the worker's network isolation ([756114e](https://github.com/Panacota96/rv-pdfauth/commit/756114e5d09489bc16e42541218e0edb8be2ba47))


### Bug Fixes

* **cli:** read the certificate password without echo (rpassword) ([54847ed](https://github.com/Panacota96/rv-pdfauth/commit/54847edbfbade8ae79e24e650b8fb78652ae06a6))
* **installer:** remove the user PATH entry on uninstall ([b834e6b](https://github.com/Panacota96/rv-pdfauth/commit/b834e6b4c5c7aeadd03642b94a21f25e54cc0a2a))
* **release:** keep the product version out of the lockfile ([10135cc](https://github.com/Panacota96/rv-pdfauth/commit/10135cc6bd85e0d83b16f1da0a231a71fd35ef6a))
* **report:** cap the font table at 100 rows (full list in objects.json) ([f7efa07](https://github.com/Panacota96/rv-pdfauth/commit/f7efa07ab9741f4463ddc3c4b79822b935b17510))

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

### Documentation
- New `docs/theory.md`: the theory behind every process in two levels (clients, lawyers and judges; examiners and
  opposing experts), with byte-layout diagrams of incremental updates and ByteRange, worked hash and hash-chain
  examples, exact thresholds of the pixel diff, and references; figures generated by
  `tests/make_theory_figures.py`.
- New `docs/admissibility.md`: reliability argument and Colombian, French/EU, ISO and US/UK requirements mapped to
  tool features, with verification marks, a consolidated gap list (G1–G26) and an examiner checklist; for counsel
  review.
- New `docs/installation.md`: Windows 10/11 installation (portable zip, per-user installer), release verification
  (SHA256SUMS, provenance attestation, Authenticode, PDFium SHA-256 and version), offline use, building from
  source, uninstalling.
- New `docs/usage.md`: end-to-end case walkthrough (case folder: rv-hashinv intake, then `rv-pdfauth analyze --case-dir`),
  outputs, finding-by-finding interpretation, timestamps, report signing, run verification, fingerprint database,
  troubleshooting.
- README, architecture, forensic-audit, reports and GUI pages aligned with the code (PDF findings catalogue,
  timestamp imprint, audit actions, visual-diff thresholds, ISO/IEC 27037 clause numbers); ADR 0001 implementation
  status notes.
