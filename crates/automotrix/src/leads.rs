//! WHAT: opening, reusing and closing out leads.
//! WHY:  a lead is the product of this whole system, so the rules about when one
//!       exists live in one place rather than being re-derived at each trigger.
//! HOW:  a conversation may have at most one OPEN lead ('new' or 'sent'); a
//!       partial unique index enforces it. open_or_reuse returns the existing
//!       one rather than racing against that index.

use anyhow::Result;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Statuses that mean "this conversation already has a lead someone will act on".
pub const OPEN_STATUSES: [&str; 2] = ["new", "sent"];

/// Returns the conversation's open lead, creating one if there is none.
///
/// This is what resolves the ordering question between leads and appointments:
/// booking calls this first, inside the same transaction, so appointments.lead_id
/// is always satisfiable and can stay NOT NULL. No orphan appointment can exist.
pub async fn open_or_reuse(
    tx: &mut Transaction<'_, Postgres>,
    dealer_id: Uuid,
    conversation_id: Uuid,
    customer_id: Uuid,
) -> Result<Uuid> {
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "select id from leads
         where conversation_id = $1 and status in ('new','sent')
         order by created_at limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((id,)) = existing {
        return Ok(id);
    }

    let id = crate::new_id();
    sqlx::query(
        "insert into leads (id, dealer_id, conversation_id, customer_id, status)
         values ($1, $2, $3, $4, 'new')",
    )
    .bind(id)
    .bind(dealer_id)
    .bind(conversation_id)
    .bind(customer_id)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

pub async fn has_open_lead(db: &PgPool, conversation_id: Uuid) -> Result<bool> {
    let row: Option<(bool,)> = sqlx::query_as(
        "select true from leads where conversation_id = $1 and status in ('new','sent') limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
}

pub async fn mark_sent(db: &PgPool, lead_id: Uuid) -> Result<()> {
    sqlx::query(
        "update leads set status = 'sent', sent_at = now()
         where id = $1 and status = 'new'",
    )
    .bind(lead_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_summary(
    db: &PgPool,
    lead_id: Uuid,
    summary: Option<serde_json::Value>,
    model: &str,
) -> Result<()> {
    sqlx::query("update leads set summary = $2, summary_model = $3 where id = $1")
        .bind(lead_id)
        .bind(summary)
        .bind(model)
        .execute(db)
        .await?;
    Ok(())
}
