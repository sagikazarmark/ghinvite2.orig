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

/// The removed toolkit is not resolved anywhere in the workspace.
#[test]
fn the_workspace_does_not_resolve_the_old_toolkit() {
    let lock = include_str!("../../../Cargo.lock");
    let forbidden: Vec<_> = lock
        .lines()
        .filter_map(|line| line.strip_prefix("name = \""))
        .filter(|name| name.starts_with("gh-toolkit"))
        .collect();
    assert!(forbidden.is_empty(), "{forbidden:?}");
}

/// The direct dependencies of `manifest` that are L1 rotating credentials
/// (`rotcred-*`), by key or by `package = "rotcred-…"` rename, in any
/// dependency table (`[dependencies]`, `[dev-dependencies]`,
/// `[target.'…'.dependencies]`, `[workspace.dependencies]`,
/// `[dependencies.rotcred-core]`).
fn rotating_credential_dependencies(manifest: &str) -> Vec<String> {
    let mut section = String::new();
    let mut found = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_owned();
            let dotted = section.rsplit_once("dependencies.").map(|(_, name)| name);
            if dotted.is_some_and(|name| name.starts_with("rotcred-")) {
                found.push(section.clone());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let renamed = value.contains("package = \"rotcred-")
            || (key == "package" && value.trim().starts_with("\"rotcred-"));
        if section.contains("dependencies") && (key.starts_with("rotcred-") || renamed) {
            found.push(format!("{section}: {key}"));
        }
    }
    found
}

/// Below the product, the toolkit (`octologin`) may adapt L1's `Refresher`;
/// ghinvite's own crates never depend on L1 rotating credentials directly,
/// nor does the workspace declare them.
#[test]
fn no_ghinvite_crate_depends_on_rotating_credentials_directly() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut manifests = vec![root.join("Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let path = entry.unwrap().path();
        let ghinvite = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("ghinvite-"));
        if ghinvite && path.join("Cargo.toml").is_file() {
            manifests.push(path.join("Cargo.toml"));
        }
    }
    assert!(manifests.len() > 10, "found the crates: {manifests:?}");
    let forbidden: Vec<_> = manifests
        .iter()
        .flat_map(|path| {
            let manifest = std::fs::read_to_string(path).unwrap();
            rotating_credential_dependencies(&manifest)
                .into_iter()
                .map(move |found| format!("{}: {found}", path.display()))
        })
        .collect();
    assert!(forbidden.is_empty(), "{forbidden:?}");
}

#[test]
fn rotating_credential_dependencies_are_found_in_every_form() {
    let manifest = r#"
        [package]
        name = "rotcred-lookalike"

        [features]
        rotcred-refresh = []

        [dependencies]
        octologin.workspace = true # may itself depend on rotcred-core
        rotcred-core = "0.1"

        [target.'cfg(target_arch = "wasm32")'.dependencies]
        refresher = { package = "rotcred-worker", version = "0.1" }

        [dev-dependencies]
        rotcred-testkit.workspace = true

        [dependencies.rotcred-store]
        version = "0.1"

        [dependencies.store]
        package = "rotcred-d1"
    "#;
    assert_eq!(
        rotating_credential_dependencies(manifest),
        [
            "dependencies: rotcred-core",
            "target.'cfg(target_arch = \"wasm32\")'.dependencies: refresher",
            "dev-dependencies: rotcred-testkit.workspace",
            "dependencies.rotcred-store",
            "dependencies.store: package",
        ]
    );
    let clean = "[dependencies]\noctologin.workspace = true\n[features]\nrotcred-x = []\n";
    assert!(rotating_credential_dependencies(clean).is_empty());
}
