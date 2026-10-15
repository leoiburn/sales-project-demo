//! WHAT: sends each queued conversation (crm_sync) to Twenty CRM as a Person
//!       (the customer) and an Opportunity (this conversation), and keeps both
//!       up to date when the customer, a lead or an appointment changes later.
//! WHY:  the dealership works its pipeline in Twenty. A lead that only lives in
//!       an email gets lost; a record in the CRM gets worked.
//! HOW:  engine::crm_sweep decides WHEN (the three hand-off rules). This module
//!       only decides HOW. tick() claims due crm_sync rows with FOR UPDATE SKIP
//!       LOCKED, re-reads every fact from the database (never from the model),
//!       and upserts over Twenty's REST API. Records are found by the demo's own
//!       ids stored in custom fields (automotrixCustomerId on Person,
//!       automotrixConversationId on Opportunity), so a crash between "created in
//!       Twenty" and "marked sent here" never makes a duplicate. The custom fields
//!       are created on first use, so a fresh Twenty workspace needs no setup.
//!       Off unless TWENTY_API_KEY is set.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use uuid::Uuid;

/// After this many failed attempts a row is parked as 'failed'. With the
/// backoff below that is roughly three hours of Twenty being down.
const MAX_ATTEMPTS: i32 = 8;

/// Custom fields this module writes, per Twenty object: (name, type, label).
const PERSON_FIELDS: [(&str, &str, &str); 4] = [
    ("automotrixCustomerId", "TEXT", "Automotrix customer id"),
    ("smsOptIn", "BOOLEAN", "SMS opt-in"),
    ("emailOptIn", "BOOLEAN", "Email opt-in"),
    ("preferredLanguage", "TEXT", "Preferred language"),
];
const OPPORTUNITY_FIELDS: [(&str, &str, &str); 7] = [
    ("automotrixConversationId", "TEXT", "Automotrix conversation id"),
    ("automotrixReason", "TEXT", "Sent to CRM because"),
    ("chatChannel", "TEXT", "Chat channel"),
    ("appointmentAt", "DATE_TIME", "Appointment"),
    ("appointmentDetails", "TEXT", "Appointment details"),
    ("vehicleOfInterest", "TEXT", "Vehicle of interest"),
    ("leadSummary", "TEXT", "Lead summary"),
];

pub struct Config {
    /// where the server reaches Twenty (http://host.docker.internal:3000 in Docker)
    pub url: String,
    pub api_key: String,
    /// where a browser reaches Twenty, for links in the operator panel
    pub public_url: String,
}

impl Config {
    /// None (sender off) when TWENTY_API_KEY is empty or unset.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("TWENTY_API_KEY").ok().filter(|k| !k.trim().is_empty())?;
        let url = std::env::var("TWENTY_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| "http://localhost:3000".into());
        let public_url = std::env::var("TWENTY_PUBLIC_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| url.clone());
        Some(Self {
            url: url.trim_end_matches('/').to_string(),
            api_key: api_key.trim().to_string(),
            public_url: public_url.trim_end_matches('/').to_string(),
        })
    }
}

/// Link to a Twenty opportunity for the operator panel; None when the sender is off.
pub fn opportunity_url(opportunity_id: &str) -> Option<String> {
    Config::from_env().map(|c| format!("{}/object/opportunity/{opportunity_id}", c.public_url))
}

pub struct Client {
    http: reqwest::Client,
    cfg: Config,
    /// the custom fields exist; checked once per process
    schema_ready: AtomicBool,
}

impl Client {
    pub fn new(cfg: Config) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("http client");
        Self { http, cfg, schema_ready: AtomicBool::new(false) }
    }

    async fn call(&self, method: reqwest::Method, path: &str, query: &[(&str, String)], body: Option<&Value>) -> Result<Value> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.cfg.url))
            .bearer_auth(&self.cfg.api_key)
            .query(query);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.with_context(|| format!("Twenty {method} {path}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Twenty {method} {path}: {status}: {}", text.chars().take(300).collect::<String>());
        }
        serde_json::from_str(&text).with_context(|| format!("Twenty {method} {path}: not JSON"))
    }

    /// Creates the custom fields this module writes, if they are missing.
    async fn ensure_schema(&self) -> Result<()> {
        if self.schema_ready.load(Ordering::Relaxed) {
            return Ok(());
        }
        let objects = self
            .call(reqwest::Method::GET, "/rest/metadata/objects", &[("limit", "500".into())], None)
            .await?;
        let list = objects["data"].as_array().ok_or_else(|| anyhow!("Twenty metadata: no object list"))?;
        for (object, fields) in [("person", &PERSON_FIELDS[..]), ("opportunity", &OPPORTUNITY_FIELDS[..])] {
            let obj = list
                .iter()
                .find(|o| o["nameSingular"] == object)
                .ok_or_else(|| anyhow!("Twenty has no {object} object"))?;
            let existing: Vec<&str> = obj["fields"]
                .as_array()
                .map(|f| f.iter().filter_map(|f| f["name"].as_str()).collect())
                .unwrap_or_default();
            for (name, kind, label) in fields {
                if existing.contains(name) {
                    continue;
                }
                let mut field = json!({
                    "objectMetadataId": obj["id"], "type": kind, "name": name, "label": label,
                    "description": "Written by the Automotrix sales demo",
                });
                if *kind == "BOOLEAN" {
                    field["defaultValue"] = json!(false);
                }
                self.call(reqwest::Method::POST, "/rest/metadata/fields", &[], Some(&field)).await?;
                tracing::info!(object, field = name, "created Twenty custom field");
            }
        }
        self.schema_ready.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// The first live record whose `field` equals `value`, as (id, record).
    async fn find(&self, plural: &str, field: &str, value: &str) -> Result<Option<(String, Value)>> {
        let v = self
            .call(
                reqwest::Method::GET,
                &format!("/rest/{plural}"),
                &[("filter", format!("{field}[eq]:\"{value}\"")), ("limit", "1".into())],
                None,
            )
            .await?;
        Ok(v["data"][plural].as_array().and_then(|a| a.first()).and_then(|r| {
            r["id"].as_str().map(|id| (id.to_string(), r.clone()))
        }))
    }

    /// Creates a record and returns its id. Twenty answers {data: {createX: {...}}}.
    async fn create(&self, plural: &str, body: &Value) -> Result<String> {
        let v = self.call(reqwest::Method::POST, &format!("/rest/{plural}"), &[], Some(body)).await?;
        v["data"]
            .as_object()
            .and_then(|d| d.values().next())
            .and_then(|r| r["id"].as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Twenty create {plural}: no id in response"))
    }

    async fn update(&self, plural: &str, id: &str, body: &Value) -> Result<()> {
        self.call(reqwest::Method::PATCH, &format!("/rest/{plural}/{id}"), &[], Some(body)).await?;
        Ok(())
    }
}

/// Everything sent to Twenty for one conversation, read from the database.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub customer_id: Uuid,
    pub conversation_id: Uuid,
    pub reason: String,
    pub channel: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub preferred_language: Option<String>,
    pub sms_opt_in: bool,
    pub email_opt_in: bool,
    pub appointment: Option<Appointment>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Appointment {
    pub kind: String,
    pub starts_at: chrono::DateTime<chrono::Utc>,
    pub timezone: String,
    pub resource: String,
    pub vehicle: Option<String>,
    pub list_price_cents: Option<i64>,
}

type ApptRow = (String, chrono::DateTime<chrono::Utc>, String, String, Option<String>, Option<i64>);

async fn load(db: &PgPool, conversation_id: Uuid, reason: &str) -> Result<Profile> {
    #[allow(clippy::type_complexity)]
    let (customer_id, channel, first_name, last_name, email, phone, preferred_language, sms_opt_in, email_opt_in): (
        Uuid, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, bool, bool,
    ) = sqlx::query_as(
        "select cu.id, c.channel, cu.first_name, cu.last_name, cu.email, cu.phone,
                cu.preferred_language, cu.sms_opt_in, cu.email_opt_in
         from conversations c join customers cu on cu.id = c.customer_id
         where c.id = $1",
    )
    .bind(conversation_id)
    .fetch_one(db)
    .await?;

    // the latest confirmed booking of this conversation, with the car if one was picked
    let appointment: Option<ApptRow> = sqlx::query_as(
        "select a.kind, lower(a.slot), d.timezone, r.name,
                case when v.id is null then null else
                    concat_ws(' ', v.year::text, v.make, v.model, v.trim_level) || ' (stock ' || v.stock_number || ')' end,
                v.list_price_cents
         from appointments a
         join leads l on l.id = a.lead_id
         join dealers d on d.id = a.dealer_id
         join resources r on r.id = a.resource_id
         left join vehicles v on v.id = a.vehicle_id
         where l.conversation_id = $1 and a.status = 'confirmed'
         order by a.created_at desc
         limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(db)
    .await?;

    // the validated summary of the newest lead (summary.rs), rendered by code
    let summary: Option<(Value,)> = sqlx::query_as(
        "select summary from leads where conversation_id = $1 and summary is not null
         order by created_at desc limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(db)
    .await?;

    Ok(Profile {
        customer_id,
        conversation_id,
        reason: reason.to_string(),
        channel,
        first_name,
        last_name,
        email,
        phone,
        preferred_language,
        sms_opt_in,
        email_opt_in,
        appointment: appointment.map(|(kind, starts_at, timezone, resource, vehicle, list_price_cents)| Appointment {
            kind, starts_at, timezone, resource, vehicle, list_price_cents,
        }),
        summary: summary.map(|(s,)| crate::summary::to_text(&s)).filter(|s| !s.trim().is_empty()),
    })
}

fn reason_text(reason: &str) -> &str {
    match reason {
        "appointment" => "Appointment booked",
        "name_phone" => "Gave name and phone",
        "email_consent" => "Gave email and agreed to emails",
        other => other,
    }
}

impl Appointment {
    fn details(&self) -> String {
        let tz: chrono_tz::Tz = self.timezone.parse().unwrap_or(chrono_tz::UTC);
        let local = self.starts_at.with_timezone(&tz).format("%a %b %-d, %-I:%M %p");
        let mut s = format!("{}: {local} ({}), with {}", self.kind.replace('_', " "), self.timezone, self.resource);
        if let Some(v) = &self.vehicle {
            s.push_str(&format!(", {v}"));
        }
        s
    }
}

/// Person body. Only fields the demo knows are sent, so nothing a salesperson
/// typed into Twenty is blanked by a sync.
pub fn person_body(p: &Profile) -> Value {
    let mut b = Map::new();
    let mut name = Map::new();
    if let Some(f) = &p.first_name {
        name.insert("firstName".into(), json!(f));
    }
    if let Some(l) = &p.last_name {
        name.insert("lastName".into(), json!(l));
    }
    if !name.is_empty() {
        b.insert("name".into(), Value::Object(name));
    }
    if let Some(e) = &p.email {
        b.insert("emails".into(), json!({ "primaryEmail": e }));
    }
    // stored as E.164 (tools::normalize_phone); Twenty splits off the country code
    if let Some(ph) = &p.phone {
        b.insert("phones".into(), json!({ "primaryPhoneNumber": ph }));
    }
    if let Some(l) = &p.preferred_language {
        b.insert("preferredLanguage".into(), json!(l));
    }
    b.insert("automotrixCustomerId".into(), json!(p.customer_id.to_string()));
    b.insert("smsOptIn".into(), json!(p.sms_opt_in));
    b.insert("emailOptIn".into(), json!(p.email_opt_in));
    Value::Object(b)
}

/// Opportunity body. `existing_stage` is None on create. The stage is set on
/// create, and later only moved from New to Meeting when a booking appears;
/// a stage the salesperson changed in Twenty is never overwritten.
pub fn opportunity_body(p: &Profile, person_id: &str, existing_stage: Option<&str>) -> Value {
    let who = match (&p.first_name, &p.last_name) {
        (Some(f), Some(l)) => format!("{f} {l}"),
        (Some(n), None) | (None, Some(n)) => n.clone(),
        (None, None) => p.email.clone().or_else(|| p.phone.clone()).unwrap_or_else(|| "Chat visitor".into()),
    };
    let what = p.appointment.as_ref().and_then(|a| a.vehicle.clone()).unwrap_or_else(|| format!("{} chat", p.channel));
    let mut b = json!({
        "name": format!("{who} · {what}"),
        "pointOfContactId": person_id,
        "automotrixConversationId": p.conversation_id.to_string(),
        "automotrixReason": reason_text(&p.reason),
        "chatChannel": p.channel,
    });
    let stage = match (existing_stage, p.appointment.is_some()) {
        (None, true) | (Some("NEW"), true) => Some("MEETING"),
        (None, false) => Some("NEW"),
        _ => None,
    };
    if let Some(s) = stage {
        b["stage"] = json!(s);
    }
    if let Some(a) = &p.appointment {
        b["appointmentAt"] = json!(a.starts_at.to_rfc3339());
        b["appointmentDetails"] = json!(a.details());
        if let Some(v) = &a.vehicle {
            b["vehicleOfInterest"] = json!(v);
        }
        // the car's list price from inventory, never a figure from the chat
        if let Some(c) = a.list_price_cents {
            b["amount"] = json!({ "amountMicros": c * 10_000, "currencyCode": "USD" });
        }
    }
    if let Some(s) = &p.summary {
        b["leadSummary"] = json!(s);
    }
    b
}

/// Upserts the Person and the Opportunity; returns (person_id, opportunity_id).
pub async fn push(client: &Client, p: &Profile) -> Result<(String, String)> {
    client.ensure_schema().await?;

    let person = person_body(p);
    let person_id = match client.find("people", "automotrixCustomerId", &p.customer_id.to_string()).await? {
        Some((id, _)) => {
            client.update("people", &id, &person).await?;
            id
        }
        None => client.create("people", &person).await?,
    };

    let opportunity_id = match client
        .find("opportunities", "automotrixConversationId", &p.conversation_id.to_string())
        .await?
    {
        Some((id, existing)) => {
            client.update("opportunities", &id, &opportunity_body(p, &person_id, existing["stage"].as_str())).await?;
            id
        }
        None => client.create("opportunities", &opportunity_body(p, &person_id, None)).await?,
    };
    Ok((person_id, opportunity_id))
}

struct Job {
    id: Uuid,
    conversation_id: Uuid,
    reason: String,
    attempts: i32,
}

/// Claims due rows: new ones, retries whose wait is over, rows a dead worker
/// left in 'sending', and 'sent' rows whose facts changed after they were sent
/// (contact details, a new lead or summary, a new booking).
/// CLI conversations are test traffic and never go to the CRM.
async fn claim(db: &PgPool, limit: i64) -> Result<Vec<Job>> {
    let rows: Vec<(Uuid, Uuid, String, i32)> = sqlx::query_as(
        "with due as (
             select s.id from crm_sync s
             join customers cu on cu.id = s.customer_id
             join conversations c on c.id = s.conversation_id
             where c.channel <> 'cli'
               and ((s.status = 'pending' and s.next_attempt_at <= now())
                 or (s.status = 'sending' and s.claimed_at < now() - interval '5 minutes')
                 or (s.status = 'sent' and (
                        cu.updated_at > s.sent_at
                     or exists (select 1 from leads l where l.conversation_id = s.conversation_id
                                and coalesce(l.sent_at, l.created_at) > s.sent_at)
                     or exists (select 1 from appointments a join leads l on l.id = a.lead_id
                                where l.conversation_id = s.conversation_id and a.created_at > s.sent_at))))
             order by s.created_at
             limit $1
             for update of s skip locked
         )
         update crm_sync s set status = 'sending', claimed_at = now(),
                attempts = case when s.status = 'sent' then 1 else s.attempts + 1 end
         from due where s.id = due.id
         returning s.id, s.conversation_id, s.reason, s.attempts",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, conversation_id, reason, attempts)| Job { id, conversation_id, reason, attempts })
        .collect())
}

async fn finish(db: &PgPool, job: &Job, result: Result<(String, String)>) -> Result<()> {
    match result {
        Ok((person_id, opportunity_id)) => {
            sqlx::query(
                "update crm_sync set status = 'sent', sent_at = now(), last_error = null,
                        external_id = $2, external_person_id = $3
                 where id = $1",
            )
            .bind(job.id)
            .bind(&opportunity_id)
            .bind(&person_id)
            .execute(db)
            .await?;
            tracing::info!(conversation = %job.conversation_id, opportunity = %opportunity_id, "synced to Twenty");
        }
        Err(e) => {
            let err = format!("{e:#}");
            tracing::warn!(conversation = %job.conversation_id, attempts = job.attempts, "Twenty sync failed: {err}");
            // 2, 4, 8, 16, 30, 30... minutes; parked as 'failed' after MAX_ATTEMPTS
            sqlx::query(
                "update crm_sync set
                     status = case when $2 >= $3 then 'failed' else 'pending' end,
                     last_error = $4,
                     next_attempt_at = now() + make_interval(mins => least(power(2, $2)::int, 30))
                 where id = $1",
            )
            .bind(job.id)
            .bind(job.attempts)
            .bind(MAX_ATTEMPTS)
            .bind(err.chars().take(500).collect::<String>())
            .execute(db)
            .await?;
        }
    }
    Ok(())
}

/// Sends one conversation now, whatever its crm_sync state. Tests use it to
/// sync their own conversation without racing the server's worker.
pub async fn sync_conversation(db: &PgPool, client: &Client, conversation_id: Uuid, reason: &str) -> Result<(String, String)> {
    let profile = load(db, conversation_id, reason).await?;
    push(client, &profile).await
}

fn client() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT.get_or_init(|| Config::from_env().map(Client::new)).as_ref()
}

/// One worker pass. Does nothing when TWENTY_API_KEY is not set.
pub async fn tick(db: &PgPool) -> Result<usize> {
    let Some(client) = client() else {
        return Ok(0);
    };
    let jobs = claim(db, 10).await?;
    for job in &jobs {
        let result = match load(db, job.conversation_id, &job.reason).await {
            Ok(profile) => push(client, &profile).await,
            Err(e) => Err(e),
        };
        finish(db, job, result).await?;
    }
    Ok(jobs.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> Profile {
        Profile {
            customer_id: Uuid::nil(),
            conversation_id: Uuid::nil(),
            reason: "name_phone".into(),
            channel: "web".into(),
            first_name: Some("Maria".into()),
            phone: Some("+12105550143".into()),
            ..Default::default()
        }
    }

    fn appointment() -> Appointment {
        Appointment {
            kind: "test_drive".into(),
            starts_at: "2026-10-16T19:00:00Z".parse().unwrap(),
            timezone: "America/Chicago".into(),
            resource: "Sales floor".into(),
            vehicle: Some("2021 Toyota RAV4 XLE (stock A1)".into()),
            list_price_cents: Some(2_599_900),
        }
    }

    #[test]
    fn unknown_fields_are_left_out_not_blanked() {
        let b = person_body(&profile());
        assert_eq!(b["name"], json!({ "firstName": "Maria" }));
        assert_eq!(b["phones"]["primaryPhoneNumber"], "+12105550143");
        assert!(b.get("emails").is_none());
        assert!(b.get("preferredLanguage").is_none());
        assert_eq!(b["smsOptIn"], false);
    }

    #[test]
    fn a_booking_sets_meeting_stage_price_and_details() {
        let mut p = profile();
        p.appointment = Some(appointment());
        let b = opportunity_body(&p, "person-1", None);
        assert_eq!(b["stage"], "MEETING");
        assert_eq!(b["name"], "Maria · 2021 Toyota RAV4 XLE (stock A1)");
        assert_eq!(b["amount"]["amountMicros"], 25_999_000_000i64);
        assert_eq!(b["appointmentDetails"], "test drive: Fri Oct 16, 2:00 PM (America/Chicago), with Sales floor, 2021 Toyota RAV4 XLE (stock A1)");
        assert_eq!(b["automotrixReason"], "Gave name and phone");
    }

    #[test]
    fn a_stage_moved_by_the_salesperson_is_kept() {
        let mut p = profile();
        assert_eq!(opportunity_body(&p, "x", None)["stage"], "NEW");
        assert!(opportunity_body(&p, "x", Some("NEW")).get("stage").is_none());
        p.appointment = Some(appointment());
        assert_eq!(opportunity_body(&p, "x", Some("NEW"))["stage"], "MEETING");
        assert!(opportunity_body(&p, "x", Some("PROPOSAL")).get("stage").is_none());
    }

    #[test]
    fn no_name_falls_back_to_contact_then_visitor() {
        let mut p = profile();
        p.first_name = None;
        assert_eq!(opportunity_body(&p, "x", None)["name"], "+12105550143 · web chat");
        p.phone = None;
        assert_eq!(opportunity_body(&p, "x", None)["name"], "Chat visitor · web chat");
    }
}
