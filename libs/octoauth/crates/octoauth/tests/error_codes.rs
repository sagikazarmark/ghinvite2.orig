//! `error-codes.txt` at the workspace root lists every stable error code the
//! toolkit's crates return from a `code()` method, so adding, renaming or
//! removing one is a deliberate edit of that file (codes are append-only;
//! L2 spec ER-01, TS-18). Codes are found as `"gh:…"` string literals in the
//! sources of every crate in `crates/`, test modules excluded; format strings
//! (`"gh:unexpected:{status}"`, `Display` only) are skipped.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The workspace's `crates/` directory.
fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/octoauth has a parent")
        .to_owned()
}

/// Every `.rs` file under `dir`, except test files (`tests.rs`, `tests/`).
fn sources(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if path.is_dir() {
            if name != "tests" {
                sources(&path, files);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" {
            files.push(path);
        }
    }
}

/// The `"gh:…"` literals of `source`, before its inline test module.
fn literals(source: &str) -> Vec<&str> {
    let source = match source.find("#[cfg(test)]\nmod tests {") {
        Some(tests) => &source[..tests],
        None => source,
    };
    source
        .match_indices("\"gh:")
        .map(|(start, _)| {
            let rest = &source[start + 1..];
            &rest[..rest.find('"').expect("a closed literal")]
        })
        .filter(|literal| !literal.contains('{'))
        .collect()
}

/// ER-01's grammar: `gh:<class>(:<detail>)*`, each segment `[a-z0-9_]+`.
fn well_formed(code: &str) -> bool {
    code.split(':').skip(1).count() >= 1
        && code.split(':').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

fn in_sources() -> BTreeSet<String> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(crates()).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            sources(&src, &mut files);
        }
    }
    assert!(files.len() > 10, "found the sources: {files:?}");
    let mut codes = BTreeSet::new();
    for file in files {
        let source = std::fs::read_to_string(&file).unwrap();
        for code in literals(&source) {
            assert!(well_formed(code), "{code} in {}", file.display());
            codes.insert(code.to_owned());
        }
    }
    codes
}

#[test]
fn error_codes_txt_lists_every_code() {
    let path = crates().with_file_name("error-codes.txt");
    let listed = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = listed
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        lines, sorted,
        "error-codes.txt: keep one code per line, sorted"
    );

    let listed: BTreeSet<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    let found = in_sources();
    let added: Vec<_> = found.difference(&listed).collect();
    let removed: Vec<_> = listed.difference(&found).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "error codes changed; if deliberate (codes are append-only), update {}:\n\
         new in the sources: {added:?}\nno longer in the sources: {removed:?}",
        path.display()
    );
}

#[test]
fn the_scan_finds_codes_and_skips_formats_and_tests() {
    let source = r#"
        #[error("gh:unexpected:{status}")]
        Self::Decode => "gh:decode",
        Self::Server { .. } => "gh:api:server",
        #[cfg(test)]
        mod tests {
            assert_eq!(error.to_string(), "gh:unexpected:307");
        }
    "#
    .replace("\n        #[cfg", "\n#[cfg")
    .replace("\n        mod tests", "\nmod tests");
    assert_eq!(literals(&source), ["gh:decode", "gh:api:server"]);
    assert!(well_formed("gh:rate_limited:primary"));
    for bad in ["gh", "gh:", "gh::x", "gh:Decode", "gh:a-b"] {
        assert!(!well_formed(bad), "{bad}");
    }
}
