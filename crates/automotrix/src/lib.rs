//! WHAT: the Automotrix sales bot - conversation engine, lead capture,
//!       transcripts, summaries, email delivery and appointment booking.
//! WHY:  the chat only earns its keep if it turns into a lead the dealership can
//!       work. Everything here exists to get from "someone asked about a RAV4"
//!       to "a salesperson has their phone number and a booked test drive".
//! HOW:  the single rule the whole crate is built around - **the model never
//!       writes facts**. It calls tools with structured arguments; Rust
//!       validates those against the database and renders every price, vehicle,
//!       email and XML document from database rows. If a number appears in front
//!       of a customer, a SELECT put it there.

pub mod adf;
pub mod calendar;
pub mod db;
pub mod email;
pub mod engine;
pub mod guardrail;
pub mod leads;
pub mod llm;
pub mod outbox;
pub mod summary;
pub mod tools;
pub mod transcript;

use anyhow::{Context, Result};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Everything a request handler needs. Cloned per request; the pool and the
/// template environment are shared.
#[derive(Clone)]
pub struct App {
    pub db: PgPool,
    pub llm: llm::Client,
    pub mailer: Arc<dyn email::EmailSender>,
    pub templates: Arc<minijinja::Environment<'static>>,
    pub summary_schema: Arc<serde_json::Value>,
}

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

pub fn load_env() {
    let _ = dotenvy::from_path(repo_root().join(".env"));
}

pub fn database_url() -> Result<String> {
    load_env();
    std::env::var("DATABASE_URL").context("DATABASE_URL not set (copy .env.example to .env)")
}

/// Time-ordered ids for rows that record events. Unlike the seed data's uuid v5,
/// these are not derived from anything - each message really is new - and v7
/// keeps them roughly insertion-ordered, which is kinder to the indexes.
pub fn new_id() -> uuid::Uuid {
    uuid::Uuid::now_v7()
}
