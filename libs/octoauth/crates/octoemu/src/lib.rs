//! octoemu: a stateful local emulator of the GitHub surface a GitHub App
//! uses (REST, App and installation auth, the OAuth web flow, webhooks), for
//! tests. Native only.
//!
//! **Skeleton (GE-01).** The server, seed, virtual clock, admin API, call log
//! and fidelity manifest exist; no GitHub route does yet. Every API and web
//! route answers `501 {"message": "octoemu: not implemented", "route": …}`,
//! never 404. The plan is `docs/specs/github-emulator.md` in the ghinvite
//! repository.
//!
//! ```no_run
//! # async fn demo() -> Result<(), octoemu::StartError> {
//! use octoemu::{Emulator, seed::{Seed, User}};
//!
//! let emulator = Emulator::start(Seed::new().user(User::new(1, "octocat"))).await?;
//! let api = emulator.api_base(); // point the client under test here
//! # let _ = api;
//! emulator.advance_clock(std::time::Duration::from_secs(3600));
//! // `Date` on every response now reads an hour later.
//! # Ok(()) }
//! ```
//!
//! One port, routed by path: the API at `/`, github.com's pages at
//! `/login/oauth/*`, avatars at `/avatars/*`, and the admin API at
//! `/__octoemu/` (`health`, `state`, `seed`, `clock`, `clock/advance`,
//! `calls`, `fidelity`). Every response carries `Date` from the virtual
//! clock. The binary (`octoemu --port 0`) prints one JSON line with the base
//! URLs once it listens.
//!
//! **Fidelity.** `fidelity.toml` labels every route and modelled behaviour
//! `verified`, `documented` or `assumed` ([`fidelity`]). In refuse-assumed
//! mode ([`Builder::refuse_assumed`], or the seed's
//! `settings.refuse_assumed`) the emulator refuses `assumed` routes with a
//! 501 and omits `assumed` headers, so a test resting on a guess fails
//! loudly.
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod admin;
mod api;
pub mod calls;
mod clock;
mod emulator;
pub mod fidelity;
mod response;
pub mod seed;
mod server;
mod state;

pub use emulator::{Builder, Emulator, StartError};

#[cfg(test)]
mod tests;
