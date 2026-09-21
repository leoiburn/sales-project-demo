//! WHAT: renders a conversation as plain text or JSON.
//! WHY:  the transcript goes into the lead email and is what gets shown if a
//!       customer ever disputes what the bot said. It is built by code from the
//!       `messages` table - the model is never asked to summarize or reproduce
//!       it, because a model asked to reproduce a transcript will smooth it.
//! HOW:  timestamps converted to the dealer's timezone, speakers labelled, text
//!       left in whatever language it was said in. Vehicles referenced by a
//!       message are looked up and rendered from the database, so the line
//!       "[Shown: 2021 Toyota RAV4 XLE, Stock A1042]" is a fact, not a memory.

use anyhow::Result;
use chrono::TimeZone;
use chrono_tz::Tz;
use serde::Serialize;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::db::{self, Message, Vehicle};

#[derive(Debug, Clone, Serialize)]
pub struct Line {
    pub message_id: Uuid,
    pub speaker: String,
    pub at: String,
    pub text: String,
    pub shown: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub conversation_id: Uuid,
    pub timezone: String,
    pub lines: Vec<Line>,
}

fn speaker(role: &str) -> &'static str {
    match role {
        "customer" => "Customer",
        "assistant" => "Assistant",
        "staff" => "Staff",
        _ => "System",
    }
}

pub async fn build(db: &PgPool, conversation_id: Uuid, dealer_id: Uuid, tz: Tz) -> Result<Transcript> {
    let messages = db::messages(db, conversation_id).await?;
    build_from(db, conversation_id, dealer_id, tz, &messages).await
}

pub async fn build_from(
    db: &PgPool,
    conversation_id: Uuid,
    dealer_id: Uuid,
    tz: Tz,
    messages: &[Message],
) -> Result<Transcript> {
    let all_ids: Vec<Uuid> = {
        let mut v: Vec<Uuid> = messages.iter().flat_map(|m| m.vehicle_ids.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    let vehicles: HashMap<Uuid, Vehicle> = db::vehicles_by_ids(db, dealer_id, &all_ids)
        .await?
        .into_iter()
        .map(|v| (v.id, v))
        .collect();

    let lines = messages
        .iter()
        // system rows are bookkeeping, not conversation
        .filter(|m| m.role != "system")
        .map(|m| Line {
            message_id: m.id,
            speaker: speaker(&m.role).to_string(),
            at: tz
                .from_utc_datetime(&m.created_at.naive_utc())
                .format("%Y-%m-%d %H:%M %Z")
                .to_string(),
            text: m.content.clone(),
            shown: m
                .vehicle_ids
                .iter()
                .filter_map(|id| vehicles.get(id))
                .map(|v| format!("{}, Stock {}", v.label(), v.stock_number))
                .collect(),
        })
        .collect();

    Ok(Transcript {
        conversation_id,
        timezone: tz.to_string(),
        lines,
    })
}

impl Transcript {
    /// The form that goes at the bottom of the lead email.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(&format!("[{}] {}: {}\n", line.at, line.speaker, line.text));
            for shown in &line.shown {
                out.push_str(&format!("    [Shown: {shown}]\n"));
            }
        }
        out
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("transcript serializes")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}
