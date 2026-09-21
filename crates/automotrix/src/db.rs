//! WHAT: database access - the pool, the per-dealer settings, and the row types
//!       the rest of the crate passes around.
//! WHY:  every fact the bot states has to come from here. Keeping the row
//!       structs in one place makes it obvious which fields exist, and stops a
//!       handler inventing a "price" field that no column backs.
//! HOW:  plain sqlx queries, no ORM. Money stays in bigint cents until the very
//!       last moment - only the formatting helpers turn it into "$24,950".

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn connect(url: &str) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(10)
        .connect(url)
        .await
        .context("could not connect to Postgres")
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DealerSettings {
    pub dealer_id: Uuid,
    pub lead_email: String,
    pub crm_adf_email: Option<String>,
    pub notify_emails: Vec<String>,
    pub timezone: String,
    pub summary_language: String,
    pub appointment_minutes: i32,
    pub buffer_minutes: i32,
    pub lead_idle_minutes: i32,
    pub ai_disclosure_en: String,
    pub ai_disclosure_es: String,
}

impl DealerSettings {
    /// Parsed timezone, falling back to Chicago if the stored name is bad -
    /// a typo in config must not take the booking flow down.
    pub fn tz(&self) -> Tz {
        self.timezone
            .parse()
            .unwrap_or(chrono_tz::America::Chicago)
    }

    pub fn disclosure(&self, lang: &str) -> &str {
        if lang == "es" {
            &self.ai_disclosure_es
        } else {
            &self.ai_disclosure_en
        }
    }
}

pub async fn settings(db: &PgPool, dealer_id: Uuid) -> Result<DealerSettings> {
    sqlx::query_as::<_, DealerSettings>(
        "select dealer_id, lead_email, crm_adf_email, notify_emails, timezone,
                summary_language, appointment_minutes, buffer_minutes,
                lead_idle_minutes, ai_disclosure_en, ai_disclosure_es
         from dealer_settings where dealer_id = $1",
    )
    .bind(dealer_id)
    .fetch_one(db)
    .await
    .context("dealer_settings row missing - run the seed loader")
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Dealer {
    pub id: Uuid,
    pub name: String,
    pub phone: Option<String>,
    pub address: Option<String>,
}

pub async fn dealer(db: &PgPool, dealer_id: Uuid) -> Result<Dealer> {
    sqlx::query_as::<_, Dealer>("select id, name, phone, address from dealers where id = $1")
        .bind(dealer_id)
        .fetch_one(db)
        .await
        .context("dealer not found")
}

/// The only dealer in the demo. A real deployment would resolve this per request.
pub async fn default_dealer(db: &PgPool) -> Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as("select id from dealers order by created_at limit 1")
        .fetch_one(db)
        .await
        .context("no dealer loaded - run scripts/reset_db.sh")?;
    Ok(row.0)
}

/// A vehicle as the customer may see it. Every customer-facing vehicle detail
/// in an email, an answer or an ADF document is rendered from one of these.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Vehicle {
    pub id: Uuid,
    pub stock_number: String,
    pub vin: String,
    pub year: i16,
    pub make: String,
    pub model: String,
    pub trim_level: Option<String>,
    pub condition: String,
    pub status: String,
    pub body_type: String,
    pub mileage: i32,
    pub list_price_cents: i64,
    pub msrp_cents: Option<i64>,
    pub exterior_color: Option<String>,
    pub drivetrain: Option<String>,
    pub fuel_type: Option<String>,
}

impl Vehicle {
    pub fn label(&self) -> String {
        match &self.trim_level {
            Some(t) => format!("{} {} {} {}", self.year, self.make, self.model, t),
            None => format!("{} {} {}", self.year, self.make, self.model),
        }
    }

    pub fn price(&self) -> String {
        money(self.list_price_cents)
    }
}

/// bigint cents -> "$24,950". The only place cents become a displayable string.
pub fn money(cents: i64) -> String {
    let dollars = cents / 100;
    let mut s = String::new();
    let digits = dollars.abs().to_string();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            s.push(',');
        }
        s.push(c);
    }
    let cent_part = (cents % 100).abs();
    if cent_part == 0 {
        format!("${s}")
    } else {
        format!("${s}.{cent_part:02}")
    }
}

const VEHICLE_COLS: &str = "id, stock_number, vin, year, make, model, trim_level,
    condition, status, body_type, mileage, list_price_cents, msrp_cents,
    exterior_color, drivetrain, fuel_type";

pub async fn vehicle(db: &PgPool, dealer_id: Uuid, id: Uuid) -> Result<Option<Vehicle>> {
    let sql = format!("select {VEHICLE_COLS} from vehicles where dealer_id = $1 and id = $2");
    Ok(sqlx::query_as::<_, Vehicle>(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(dealer_id)
        .bind(id)
        .fetch_optional(db)
        .await?)
}

pub async fn vehicles_by_ids(db: &PgPool, dealer_id: Uuid, ids: &[Uuid]) -> Result<Vec<Vehicle>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let sql =
        format!("select {VEHICLE_COLS} from vehicles where dealer_id = $1 and id = any($2) order by list_price_cents");
    Ok(sqlx::query_as::<_, Vehicle>(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(dealer_id)
        .bind(ids)
        .fetch_all(db)
        .await?)
}

pub async fn vehicle_by_stock(
    db: &PgPool,
    dealer_id: Uuid,
    stock_number: &str,
) -> Result<Option<Vehicle>> {
    let sql =
        format!("select {VEHICLE_COLS} from vehicles where dealer_id = $1 and stock_number = $2");
    Ok(sqlx::query_as::<_, Vehicle>(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(dealer_id)
        .bind(stock_number)
        .fetch_optional(db)
        .await?)
}

pub async fn primary_photo(db: &PgPool, vehicle_id: Uuid) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "select storage_path from vehicle_photos where vehicle_id = $1 and is_primary limit 1",
    )
    .bind(vehicle_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| r.0))
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Customer {
    pub id: Uuid,
    pub dealer_id: Uuid,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub preferred_language: Option<String>,
    pub sms_opt_in: bool,
}

impl Customer {
    pub fn full_name(&self) -> Option<String> {
        match (&self.first_name, &self.last_name) {
            (Some(f), Some(l)) => Some(format!("{f} {l}")),
            (Some(f), None) => Some(f.clone()),
            (None, Some(l)) => Some(l.clone()),
            (None, None) => None,
        }
    }

    pub fn reachable(&self) -> bool {
        self.email.is_some() || self.phone.is_some()
    }
}

pub const CUSTOMER_COLS: &str =
    "id, dealer_id, first_name, last_name, email, phone, preferred_language, sms_opt_in";

pub async fn customer(db: &PgPool, id: Uuid) -> Result<Customer> {
    let sql = format!("select {CUSTOMER_COLS} from customers where id = $1");
    Ok(
        sqlx::query_as::<_, Customer>(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(id)
            .fetch_one(db)
            .await?,
    )
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Conversation {
    pub id: Uuid,
    pub dealer_id: Uuid,
    pub customer_id: Uuid,
    pub channel: String,
    pub status: String,
    pub bot_paused: bool,
    pub last_message_at: DateTime<Utc>,
}

/// Finds the conversation behind a channel identity, or starts one. This is how
/// a returning web visitor is reconnected to their own history: the browser
/// keeps a session id, and that id maps back to the same customer row.
pub async fn conversation_for_identity(
    db: &PgPool,
    dealer_id: Uuid,
    channel: &str,
    external_id: &str,
) -> Result<Conversation> {
    let existing: Option<Conversation> = sqlx::query_as(
        "select c.id, c.dealer_id, c.customer_id, c.channel, c.status, c.bot_paused,
                c.last_message_at
         from conversations c
         join customer_identities i on i.customer_id = c.customer_id
         where i.channel = $1 and i.external_id = $2 and c.status <> 'closed'
         order by c.started_at desc limit 1",
    )
    .bind(channel)
    .bind(external_id)
    .fetch_optional(db)
    .await?;
    if let Some(c) = existing {
        return Ok(c);
    }

    let mut tx = db.begin().await?;
    let customer_id = crate::new_id();
    sqlx::query("insert into customers (id, dealer_id) values ($1, $2)")
        .bind(customer_id)
        .bind(dealer_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "insert into customer_identities (id, customer_id, dealer_id, channel, external_id)
         values ($1, $2, $3, $4, $5)",
    )
    .bind(crate::new_id())
    .bind(customer_id)
    .bind(dealer_id)
    .bind(channel)
    .bind(external_id)
    .execute(&mut *tx)
    .await?;

    let convo: Conversation = sqlx::query_as(
        "insert into conversations (id, dealer_id, customer_id, channel)
         values ($1, $2, $3, $4)
         returning id, dealer_id, customer_id, channel, status, bot_paused, last_message_at",
    )
    .bind(crate::new_id())
    .bind(dealer_id)
    .bind(customer_id)
    .bind(channel)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(convo)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Message {
    pub id: Uuid,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<serde_json::Value>,
    pub vehicle_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Appends one message. There is no update path on purpose - a trigger rejects
/// UPDATE and DELETE on this table, because the transcript is evidence.
pub async fn append_message(
    exec: impl sqlx::PgExecutor<'_>,
    conversation_id: Uuid,
    dealer_id: Uuid,
    role: &str,
    content: &str,
    tool_calls: Option<serde_json::Value>,
    vehicle_ids: &[Uuid],
) -> Result<Uuid> {
    let id = crate::new_id();
    sqlx::query(
        "insert into messages (id, conversation_id, dealer_id, role, content,
             tool_calls, vehicle_ids)
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(conversation_id)
    .bind(dealer_id)
    .bind(role)
    .bind(content)
    .bind(tool_calls)
    .bind(vehicle_ids)
    .execute(exec)
    .await?;
    Ok(id)
}

pub async fn touch_conversation(exec: impl sqlx::PgExecutor<'_>, id: Uuid) -> Result<()> {
    sqlx::query("update conversations set last_message_at = now() where id = $1")
        .bind(id)
        .execute(exec)
        .await?;
    Ok(())
}

pub async fn messages(db: &PgPool, conversation_id: Uuid) -> Result<Vec<Message>> {
    Ok(sqlx::query_as::<_, Message>(
        "select id, role, content, tool_calls, vehicle_ids, created_at
         from messages where conversation_id = $1 order by created_at, id",
    )
    .bind(conversation_id)
    .fetch_all(db)
    .await?)
}

/// Every vehicle actually shown in this conversation. The summary validator uses
/// this to drop any vehicle the model claims interest in that was never shown.
pub async fn shown_vehicle_ids(db: &PgPool, conversation_id: Uuid) -> Result<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "select distinct unnest(vehicle_ids) from messages where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub async fn record_guardrail_event(
    db: &PgPool,
    conversation_id: Option<Uuid>,
    dealer_id: Uuid,
    kind: &str,
    detail: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "insert into guardrail_events (id, conversation_id, dealer_id, kind, detail)
         values ($1, $2, $3, $4, $5)",
    )
    .bind(crate::new_id())
    .bind(conversation_id)
    .bind(dealer_id)
    .bind(kind)
    .bind(detail)
    .execute(db)
    .await?;
    Ok(())
}
