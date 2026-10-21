//! RV PDFAuth core library.
//!
//! The core does all the work and never prints: front-ends (CLI, GUI) call [`run::run`] and
//! render the [`events::Event`] stream it emits.
//!
//! ```text
//! input ──► hash (1–2 algorithms, one read) ──► analyzer ──► duplicates ──► reports
//!   └──────────────── every step appended to the hash-chained audit log ───────────┘
//! ```

pub mod analyzer;
pub mod audit;
pub mod env;
pub mod events;
pub mod fingerprints;
pub mod hash;
pub mod i18n;
pub mod input;
pub mod pdfscan;
pub mod report;
pub mod run;
pub mod time;
pub mod trust;
pub mod visual;
pub mod worker;

pub use events::Event;
pub use hash::HashAlgo;
pub use run::{RunConfig, RunSummary, run};

pub const TOOL_NAME: &str = "rv-pdfauth";
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
