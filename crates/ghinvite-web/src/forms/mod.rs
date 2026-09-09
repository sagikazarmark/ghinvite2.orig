//! Server-side handling of the Console's server-rendered forms.
//!
//! Every mutation is a native `<form method="post">` and the server is the
//! sole validation authority (ADR 0001). Each submodule owns one form's raw
//! POST body: it parses it into the shared typed model from `ghinvite-ui`,
//! runs that model's validators through `dioform-core`, and hands the route
//! either data the command facade may act on or the structured errors the
//! view renders inline. Routes stay shallow; views stay presentational; the
//! rules live once, in the shared model.

pub mod create_link;
