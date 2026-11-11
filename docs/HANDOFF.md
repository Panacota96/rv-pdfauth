# Handover — rv-pdfauth 0.4.1

State on 2026-11-11 (commit `6731c48` on `main`). **rv-pdfauth** — analysis: signals of modification in PDF documents (revisions, metadata, fonts, signatures, hidden content).

This page is for whoever picks the project up next: where things are, how to build, test and release it, the decisions already made and what is
still open. The user-facing guides are [usage.md](usage.md) and [installation.md](installation.md).

```mermaid
flowchart LR
    S["src/core<br/>(all logic)"] --> C["src/cli"]
    S --> G["src/gui"]
    S --> T["tests + fixtures"]
    C & G --> R["release.yml<br/>signed exe, installer, zip,<br/>SBOM, SHA256SUMS"]
    F["rakvor-tool-forge<br/>(template, validator)"] -. "shared rules" .-> S
```

## 1. State at handover

| Item | Value |
| --- | --- |
| Version | 0.4.1 (release-please, `version.txt`) |
| Toolchain | Rust pinned in `rust-toolchain.toml`; `rust-version` = 1.88 (minimum) |
| CI | `ci.yml`: Linux + Windows tests, fmt, clippy `-D warnings`, `cargo deny check advisories licenses bans sources`, docs drift |
| Releases | signed with the internal Rakvor code-signing certificate; the chain ends at "Rakvor Internal Code Signing Root CA", SHA-256 `DC:9B:A1:A2:FC:D4:F0:63:3C:52:14:B7:CC:EB:25:BE:D8:F3:F7:D9:EB:AF:11:15:90:25:0C:BC:AA:83:DF:60`, valid to 2036-10-20 |
| Forge | the contract this repository follows and its validator live in [rakvor-tool-forge](https://github.com/Panacota96/rakvor-tool-forge) (current: v1.2.0, `7c3a71a`) |

## 2. Where things are

| Path | What it holds |
| --- | --- |
| `src/core` | everything that decides a result: inputs, checks, findings, reports, audit log, case folder, trust (`src/core/src/trust.rs`) |
| `src/cli`, `src/gui` | thin front ends over the core (the GUI records every user action as a `ui_*` audit entry) |
| `docs/` | theory (Part A for non-experts, Part B for experts), admissibility (Colombia, France), usage, installation, ADRs |
| `tests/fixtures/tsa-chains` | 34 TSA certificate chains with OpenSSL's verdicts (shared by every tool and both forge profiles) |
| `packaging/` | Inno Setup script, signing root (public certificate only) |
| `deny.toml` | licence allow-list (no GPL/AGPL/LGPL), sources, advisory exceptions with reasons |

**Shared code.** `src/core/src/trust.rs` is byte-identical in rv-hub, rv-hashinv, rv-rsmf, rv-certcheck, rv-imgauth,
rv-pdfauth and the forge template. Change it in all of them in one go (and `core/trust.py` of the forge's Python
template, graded by the same fixtures). The installer AppId table in `src/cli/tests/packaging.rs` lists every
tool; a new tool's AppId goes into every repository's copy.

## 3. Everyday commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
export RV_PDFIUM_PATH=$PWD/third_party/pdfium/linux-x64/libpdfium.so   # PDFium for the tests
cargo deny check                                    # advisories, licences, bans, sources
python <forge>/scripts/validate_project.py . --run --tests   # the Rakvor contract (forge checkout)
```

`docs/usage-cli.md` is generated from `--help` (`<tool> docs-cli`); a test fails when it drifts.

## 4. Releasing

Conventional Commits drive release-please: `feat:` minor, `fix:` patch, `docs:`/`test:`/`chore:` no release.
Merging the release PR tags the version and the release workflow builds, signs, packages and uploads. Full steps,
secrets (`SIGN_PFX_BASE64`, `SIGN_PFX_PASSWORD`) and the manual fallback: [release-process.md](release-process.md).

Practical notes from this project:

- CI on release-please's own PRs can stop at *action required*: approve the run (Actions tab) before merging.
- After a release, verify it independently: `SHA256SUMS` over the downloaded assets, and every `.exe` (installer,
  CLI, GUI) chaining to the root above (`signtool verify /pa /v` on Windows, or any Authenticode verifier).
- Repository settings: enable *Automatically delete head branches*; merged branches are not deleted otherwise.

## 5. Decisions

| ADR | Decision |
| --- | --- |
| [0001](adr/0001-language.md) | Implementation language |
| [0002](adr/0002-tsa-token-verification.md) | How RFC 3161 tokens and TSA chains are verified |
| [0003](adr/0003-dependency-advisories.md) | Dependency advisories, exceptions and RSA blinding |

Standing rules: no private key, password or real personal data in the repository; secrets only through environment
variables or prompts and never logged; no GPL/AGPL/LGPL components; tools report signals, not verdicts.

## 6. Open work

- PDFium is a pinned binary dependency (`third_party/pdfium`, version and SHA-256 in `PDFIUM_VERSION.txt`, licences in `NOTICE`), loaded at run time; tests need `RV_PDFIUM_PATH` pointing at the Linux library.
- The licence (`LICENSE`) and the admissibility analysis (`docs/admissibility.md`) are drafts pending a lawyer's review (Colombia and France).
- Revocation of the TSA certificate is never checked by the tools (gap G18): it is established separately from CRL/OCSP evidence covering genTime ([ADR 0002](adr/0002-tsa-token-verification.md)).
- RUSTSEC-2023-0071 is a documented exception until a stable `cms` 0.3 (rsa 0.10) exists ([ADR 0003](adr/0003-dependency-advisories.md)).

The gap table in [admissibility.md](admissibility.md) lists every known limitation with its owner (engineering,
counsel, examiner, firm).
