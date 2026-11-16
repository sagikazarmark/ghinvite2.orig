//! Pure, secret-safe deployment configuration shared by native and Worker roots.
//!
//! This crate has no web, storage, workflow-runtime or environment dependency.
//! Composition roots supply their own variables/secrets source and explicitly
//! select the deployment stage before publishing a validated configuration.
pub mod apps;

pub mod bulk;

pub mod github;

pub mod reissue;
