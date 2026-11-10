//! Cloudflare Workers apps use the `worker` crate (0.8), which links
//! wasm-streams 0.5. reqwest 0.12's `stream` feature links wasm-streams 0.4,
//! and the two export the same wasm-bindgen symbols, so a Worker with both
//! fails to link (`duplicate symbol: intounderlyingsource_pull`). No crate in
//! this workspace may pull wasm-streams into a wasm32 build.

#[test]
fn wasm32_builds_do_not_link_wasm_streams() {
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            "wasm32-unknown-unknown",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo runs");
    assert!(output.status.success(), "cargo metadata failed");
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let linked: Vec<&str> = metadata["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|node| node["id"].as_str())
        .filter(|id| id.contains("wasm-streams"))
        .collect();
    assert!(linked.is_empty(), "wasm32 builds link {linked:?}");
}
