//! octoemu's fidelity suite (`docs/specs/github-emulator.md` §4, GE-08):
//! one set of scenarios run against an embedded octoemu
//! ([`Target::Emulator`]) or real GitHub ([`Target::Live`]).
//!
//! - **Scenarios** ([`scenarios`]) use only public GitHub routes; the
//!   emulator's clock is the one exception. Each has a stable ID that
//!   octoemu's `fidelity.toml` `[scenarios]` table maps to the entries it
//!   measures, so a live run can stamp them `verified`.
//! - **Snapshots** ([`snapshot`]): status, the headers we rely on and the
//!   body, normalised ([`normalise`]: IDs, timestamps, tokens, GUIDs,
//!   origins and the world's names become placeholders), one file per
//!   scenario under `snapshots/`. Compare mode checks a run against them;
//!   record mode writes them. Field order is ignored; fields one side lacks
//!   are listed, not failures.
//! - **Schema check** ([`schema`]): every response is validated against
//!   the pinned subset of GitHub's OpenAPI 3.1 description
//!   (`schemas/rest-api-subset.json`).
//!
//! Run it:
//!
//! ```text
//! cargo test -p octoemu-conformance                         # emulator, compare
//! OCTOEMU_CONFORMANCE=record cargo test -p octoemu-conformance --test emulator
//! OCTOEMU_LIVE_ENV=/outside/the/repo/live.env \
//!   cargo test -p octoemu-conformance --test live -- --ignored --nocapture
//! ```
//!
//! Native only; on wasm32 the crate is empty.
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod ctx;
pub mod normalise;
pub mod runner;
pub mod scenarios;
pub mod schema;
pub mod snapshot;
pub mod target;

pub use runner::{Mode, Options, Outcome, Report, run};
pub use target::{Target, TargetKind};
