//! WHAT: helpers shared by the two seed-data generators.
//! WHY:  both resolve files relative to the repo root and hash content the same
//!       way; keeping that in one place stops them drifting apart.
//! HOW:  repo root comes from AUTOMOTRIX_ROOT if set, otherwise from this
//!       crate's compile-time location, same rule as the other crates.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn repo_root() -> PathBuf {
    if let Ok(root) = std::env::var("AUTOMOTRIX_ROOT") {
        if !root.trim().is_empty() {
            return PathBuf::from(root);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

pub fn path(rel: &str) -> PathBuf {
    repo_root().join(rel)
}

pub fn sha256_hex(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Every cars/<id>/specs.json, sorted by folder name so output order is stable.
pub fn spec_paths() -> anyhow::Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(path("cars"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("specs.json"))
        .filter(|p| p.exists())
        .collect();
    out.sort();
    Ok(out)
}

/// 45021 -> "45,021"
pub fn thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(45021), "45,021");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
