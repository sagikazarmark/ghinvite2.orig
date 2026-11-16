//! octoemu: a stateful local emulator of the GitHub surface a GitHub App
//! uses (REST, App and installation auth, the OAuth web flow, webhooks), for
//! tests. Native only.
//!
//! **Organizations (GE-06).** The server, seed, virtual clock, admin API,
//! call log and fidelity manifest (GE-01); App JWTs, stateless installation
//! tokens with scoping, revocation and suspension, per-route credential and
//! permission checks, and the `/app…` and `/installation…` routes (GE-02);
//! repositories, collaborators, permissions, repository invitations (expiry
//! and the daily limit on the clock), users, avatars, the invitee's side
//! through the admin API and the webhook outbox ([`events`]) (GE-03);
//! webhook delivery, the deliveries API and installation events
//! ([`webhooks`]) (GE-04); the OAuth web flow (the authorize page and the
//! token endpoint, PKCE, refresh), user access tokens and classic PATs,
//! `/user`, `/user/emails`, `/applications/{client_id}/token|grant` and the
//! invitee's `/user/repository_invitations` routes (GE-05); organizations,
//! memberships, organization invitations (expiry, the failed list, the
//! budget, seats), teams and team memberships with repository grants, the
//! user's `/user/memberships/orgs` routes and `organization` / `membership`
//! events (GE-06). Every
//! other API and web route answers
//! `501 {"message": "octoemu: not implemented", "route": …}`, never 404. The
//! plan is `docs/specs/github-emulator.md` in the ghinvite repository.
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
//! `calls`, `fidelity`, `tokens/installation`, `tokens/user`, `oauth…`,
//! `installations…`,
//! `apps/{id}/webhook`, `events`, `deliveries…`, `act/{login}/…`,
//! `orgs/{org}/spammed`). Every
//! response carries `Date` from the virtual clock. The binary
//! (`octoemu --port 0`) prints one JSON line with the base URLs once it
//! listens.
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

mod access;
mod admin;
mod api;
mod applications;
mod apps;
mod auth;
pub mod calls;
mod clock;
mod collaboration;
mod emulator;
pub mod events;
pub mod fidelity;
mod form;
mod hooks;
mod invitee;
mod jwt;
mod oauth;
mod organization;
mod orgs;
mod paginate;
mod repos;
mod response;
pub mod seed;
mod server;
mod state;
mod tokens;
mod user_tokens;
mod users;
mod views;
pub mod webhooks;

pub use emulator::{Builder, Emulator, StartError};

#[cfg(test)]
mod tests;
