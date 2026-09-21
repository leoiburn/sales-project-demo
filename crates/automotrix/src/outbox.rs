//! WHAT: the queue of mail waiting to go out, and the worker that sends it.
//! WHY:  an LLM turn must never block on SMTP, and a lead must never be lost
//!       because a mail server was down. Rows are written in the SAME
//!       transaction as the lead or appointment that caused them, so either both
//!       exist or neither does - there is no state where a lead was recorded but
//!       its email was never queued.
//! HOW:  the worker claims rows with FOR UPDATE SKIP LOCKED (so several workers
//!       never grab the same row), retries with exponential backoff up to five
//!       attempts, then parks the row as 'failed' with the last error. The
//!       idempotency_key is UNIQUE, so enqueueing the same lead twice is a
//!       no-op rather than a second email.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::email::{EmailSender, OutgoingEmail};

pub const MAX_ATTEMPTS: i32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub dealer_id: Uuid,
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
}

/// Queues one message. Returns false if this idempotency key was already queued,
/// which is the normal outcome when a trigger fires twice.
pub async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    dealer_id: Uuid,
    kind: &str,
    idempotency_key: &str,
    payload: serde_json::Value,
) -> Result<bool> {
    let res = sqlx::query(
        "insert into outbox (id, dealer_id, kind, idempotency_key, payload)
         values ($1, $2, $3, $4, $5)
         on conflict (idempotency_key) do nothing",
    )
    .bind(crate::new_id())
    .bind(dealer_id)
    .bind(kind)
    .bind(idempotency_key)
    .bind(payload)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// Same as enqueue but on a pool, for callers that are not already in a
/// transaction (the idle sweep).
pub async fn enqueue_now(
    db: &PgPool,
    dealer_id: Uuid,
    kind: &str,
    idempotency_key: &str,
    payload: serde_json::Value,
) -> Result<bool> {
    let mut tx = db.begin().await?;
    let queued = enqueue(&mut tx, dealer_id, kind, idempotency_key, payload).await?;
    tx.commit().await?;
    Ok(queued)
}

/// Claims up to `limit` due jobs. SKIP LOCKED means a second worker walks past
/// rows this one already holds instead of waiting on them.
async fn claim(db: &PgPool, limit: i64) -> Result<Vec<Job>> {
    Ok(sqlx::query_as::<_, (Uuid, Uuid, String, serde_json::Value, i32)>(
        "with due as (
             select id from outbox
             where status = 'pending' and next_attempt_at <= now()
             order by next_attempt_at
             limit $1
             for update skip locked
         )
         update outbox o set status = 'sending', attempts = o.attempts + 1
         from due where o.id = due.id
         returning o.id, o.dealer_id, o.kind, o.payload, o.attempts",
    )
    .bind(limit)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|(id, dealer_id, kind, payload, attempts)| Job {
        id,
        dealer_id,
        kind,
        payload,
        attempts,
    })
    .collect())
}

async fn mark_sent(db: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("update outbox set status = 'sent', sent_at = now(), last_error = null where id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Backoff is 2^attempts minutes, so attempt 5 lands about half an hour out.
async fn mark_failed(db: &PgPool, job: &Job, err: &str) -> Result<()> {
    let give_up = job.attempts >= MAX_ATTEMPTS;
    let delay_minutes = 2_i64.pow(job.attempts.clamp(0, 6) as u32);
    sqlx::query(
        "update outbox set
             status = case when $2 then 'failed' else 'pending' end,
             next_attempt_at = now() + ($3 || ' minutes')::interval,
             last_error = $4
         where id = $1",
    )
    .bind(job.id)
    .bind(give_up)
    .bind(delay_minutes.to_string())
    .bind(err.chars().take(2000).collect::<String>())
    .execute(db)
    .await?;
    if give_up {
        tracing::error!(job = %job.id, kind = %job.kind, "outbox job failed permanently: {err}");
    } else {
        tracing::warn!(job = %job.id, attempt = job.attempts, "outbox retry in {delay_minutes}m: {err}");
    }
    Ok(())
}

/// Runs one pass. Returns how many jobs were handled, so tests can drive the
/// worker without waiting on a timer.
pub async fn tick(db: &PgPool, mailer: &dyn EmailSender) -> Result<usize> {
    let jobs = claim(db, 20).await?;
    let n = jobs.len();
    for job in jobs {
        let email: Result<OutgoingEmail> = serde_json::from_value(job.payload.clone())
            .map_err(|e| anyhow::anyhow!("malformed outbox payload: {e}"));
        let result = match email {
            Ok(e) => mailer.send(&e).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(()) => {
                mark_sent(db, job.id).await?;
                // the lead is 'sent' only once its mail really left
                if job.kind == "lead_email" {
                    if let Some(lead_id) = job
                        .payload
                        .get("lead_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| Uuid::parse_str(s).ok())
                    {
                        crate::leads::mark_sent(db, lead_id).await?;
                    }
                }
            }
            Err(e) => mark_failed(db, &job, &e.to_string()).await?,
        }
    }
    Ok(n)
}
