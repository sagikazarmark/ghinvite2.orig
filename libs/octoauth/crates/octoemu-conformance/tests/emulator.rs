//! The fidelity suite against an embedded octoemu: every scenario, compared
//! with its checked-in snapshot, every response schema-checked. Set
//! `OCTOEMU_CONFORMANCE=record` to rewrite the emulator-sourced snapshots
//! (a snapshot recorded live is never overwritten from the emulator).
#![cfg(not(target_arch = "wasm32"))]

use octoemu_conformance::{Options, Target, run};

#[tokio::test(flavor = "multi_thread")]
async fn the_emulator_matches_the_snapshots_and_the_schemas() {
    let options = Options::from_env().unwrap();
    let report = run(&Target::emulator(), &options).await.unwrap();
    println!("{}", report.text());
    octoemu_conformance::runner::write_report(&report).unwrap();
    let problems = report.problems();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
