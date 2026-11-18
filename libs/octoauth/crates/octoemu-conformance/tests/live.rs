//! The fidelity suite against real GitHub. Ignored unless asked for, and
//! then it needs `OCTOEMU_LIVE_ENV` naming the live env file (outside the
//! repository; format in the octoemu README):
//!
//! ```text
//! OCTOEMU_LIVE_ENV=/path/live.env OCTOEMU_CONFORMANCE=record \
//!   cargo test -p octoemu-conformance --test live -- --ignored --nocapture
//! ```
//!
//! `OCTOEMU_LIVE_DESTRUCTIVE=1` adds the destructive scenarios (on a
//! throwaway organization), `OCTOEMU_LIVE_SLOW=1` the slow ones.
#![cfg(not(target_arch = "wasm32"))]

use octoemu_conformance::{Options, Target, run};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "real GitHub: needs OCTOEMU_LIVE_ENV (see the octoemu README)"]
async fn live() {
    let target = Target::live_from_env().unwrap();
    let options = Options::from_env().unwrap();
    let report = run(&target, &options).await.unwrap();
    println!("{}", report.text());
    octoemu_conformance::runner::write_report(&report).unwrap();
    let problems = report.problems();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
