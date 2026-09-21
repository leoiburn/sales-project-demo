//! WHAT: helpers shared by the `load` and `verify` binaries.
//! WHY:  both need the same connection string, the same id derivation and the
//!       same repo-root resolution; duplicating them is how they drift apart.
//! HOW:  ids are uuid v5 over a fixed namespace, so the same natural key always
//!       produces the same uuid. That is what makes the loader idempotent - a
//!       second run computes the ids of the rows that are already there and the
//!       upserts become no-ops instead of duplicates.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Fixed demo namespace. Changing it re-keys every row in the database.
pub const NS: Uuid = Uuid::from_u128(0x6f1d5e7a_9b2c_4f3e_8a1d_0c5b7e9a2d41);

pub const EMBEDDING_DIM: usize = 768;

/// Deterministic id from a natural key, e.g. uid(&["vehicle", dealer, vin]).
pub fn uid(parts: &[&str]) -> Uuid {
    Uuid::new_v5(&NS, parts.join("|").as_bytes())
}

/// Repo root, resolved from the crate location so the binaries work from
/// anywhere (cargo run, ./target/release/load, the reset script).
/// Where the repo's data files live. In development that is two levels above
/// this crate, resolved from the path baked in at compile time. Inside a
/// container that build path does not exist, so AUTOMOTRIX_ROOT overrides it.
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

pub fn database_url() -> Result<String> {
    let _ = dotenvy::from_path(repo_root().join(".env"));
    std::env::var("DATABASE_URL").context("DATABASE_URL not set (copy .env.example to .env)")
}

/// Escapes one field for Postgres COPY ... FROM STDIN text format.
/// None becomes \N, which is how text format spells NULL.
pub fn copy_field(v: Option<&str>) -> String {
    match v {
        None => "\\N".to_string(),
        Some(s) => s
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t"),
    }
}

/// Builds one COPY text-format line from already-optional fields.
pub fn copy_line(fields: &[Option<String>]) -> String {
    let mut line = fields
        .iter()
        .map(|f| copy_field(f.as_deref()))
        .collect::<Vec<_>>()
        .join("\t");
    line.push('\n');
    line
}
