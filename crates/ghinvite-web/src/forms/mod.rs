//! Server-side validation for the Console's server-rendered forms.
//!
//! Every mutation is a native `<form method="post">` and the server is the
//! sole validation authority (ADR 0001). Each submodule owns one form: it
//! takes the raw POST body, applies every rule, and hands the route either
//! data the command facade may act on or the structured errors the view
//! renders inline. Routes stay shallow; views stay presentational.

pub mod create_link;
