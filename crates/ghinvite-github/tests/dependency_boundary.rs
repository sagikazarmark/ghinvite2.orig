//! Dependency rules that `scripts/check-toolkit-boundary.py` enforced while
//! ghinvite consumed gh-toolkit (removed in GM-10), restated for the code
//! that replaced it.

/// The `[...]` section headers and keys of a manifest, without a TOML
/// parser: enough for the flat manifests of this workspace.
fn dependency_keys(manifest: &str) -> Vec<(String, String)> {
    let mut section = String::new();
    let mut keys = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_owned();
        } else if let Some((key, _)) = line.split_once('=')
            && section.contains("dependencies")
        {
            keys.push((section.clone(), key.trim().to_owned()));
        }
    }
    keys
}

/// The GitHub client sits below the product: product crates depend on it,
/// never the reverse (its own dev-dependency on itself aside).
#[test]
fn the_github_client_depends_on_no_product_crate() {
    let manifest = include_str!("../Cargo.toml");
    let product: Vec<_> = dependency_keys(manifest)
        .into_iter()
        .filter(|(_, key)| key.starts_with("ghinvite-") && key != "ghinvite-github")
        .collect();
    assert!(product.is_empty(), "{product:?}");
}

/// Neither the removed toolkit nor L1 rotating credentials is resolved
/// anywhere in the workspace.
#[test]
fn the_workspace_resolves_neither_the_old_toolkit_nor_rotating_credentials() {
    let lock = include_str!("../../../Cargo.lock");
    let forbidden: Vec<_> = lock
        .lines()
        .filter_map(|line| line.strip_prefix("name = \""))
        .filter(|name| name.starts_with("gh-toolkit") || name.starts_with("rotcred-"))
        .collect();
    assert!(forbidden.is_empty(), "{forbidden:?}");
}
