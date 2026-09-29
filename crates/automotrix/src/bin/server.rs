//! WHAT: the HTTPS server - web chat, its JSON API, and the background worker.
//! WHY:  so the bot can be tried from a browser on this machine, or from a phone
//!       on the same network, exactly as a customer would use it.
//! HOW:  axum over rustls. On first start it generates a self-signed certificate
//!       for localhost and this machine's LAN addresses and keeps it in ./certs,
//!       so the browser warning only has to be accepted once. The outbox worker
//!       runs in the same process as a tokio task.

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use automotrix::calendar::{CalendarProvider, LocalCalendar};
use automotrix::{db, email, engine, llm, summary, App};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::services::ServeDir;

#[derive(Deserialize)]
struct ChatIn {
    session_id: String,
    text: String,
}

#[derive(Deserialize)]
struct HistoryQ {
    session_id: String,
}

/// Session ids come from the browser. Bound their shape so a hostile client
/// cannot stuff arbitrary text into customer_identities.external_id.
fn valid_session(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

async fn vehicle_cards(app: &App, vehicles: &[db::Vehicle]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for v in vehicles {
        let photo = db::primary_photo(&app.db, v.id).await.ok().flatten();
        out.push(json!({
            "id": v.id, "label": v.label(), "price": v.price(), "mileage": v.mileage,
            "condition": v.condition, "stock": v.stock_number, "photo": photo,
        }));
    }
    out
}

async fn chat(State(app): State<App>, Json(body): Json<ChatIn>) -> impl IntoResponse {
    if !valid_session(&body.session_id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad session id" })));
    }
    let text = body.text.trim();
    if text.is_empty() || text.len() > 2000 {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "message must be 1-2000 characters" })));
    }
    if !llm::Client::configured() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "The AI is not configured yet: set ANTHROPIC_API_KEY in .env and restart." })),
        );
    }

    let result = async {
        let dealer_id = db::default_dealer(&app.db).await?;
        let convo = db::conversation_for_identity(&app.db, dealer_id, "web", &body.session_id).await?;
        engine::turn(&app, &convo, text).await
    }
    .await;

    match result {
        Ok(reply) => {
            let cards = vehicle_cards(&app, &reply.vehicles).await;
            (
                StatusCode::OK,
                Json(json!({ "text": reply.text, "vehicles": cards, "handoff": reply.handoff })),
            )
        }
        Err(e) => {
            tracing::error!("chat turn failed: {e:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Sorry, something went wrong on our side. Please try again." })),
            )
        }
    }
}

/// Lets a returning visitor see their earlier conversation. The browser holds
/// the session id; this is what makes the chat's memory survive a closed tab.
async fn history(State(app): State<App>, Query(q): Query<HistoryQ>) -> impl IntoResponse {
    if !valid_session(&q.session_id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad session id" })));
    }
    let found: Option<(uuid::Uuid, uuid::Uuid, bool)> = sqlx::query_as(
        "select c.id, c.dealer_id, c.bot_paused from conversations c
         join customer_identities i on i.customer_id = c.customer_id
         where i.channel = 'web' and i.external_id = $1 and c.status <> 'closed'
         order by c.started_at desc limit 1",
    )
    .bind(&q.session_id)
    .fetch_optional(&app.db)
    .await
    .ok()
    .flatten();

    let Some((convo_id, dealer_id, paused)) = found else {
        return (StatusCode::OK, Json(json!({ "messages": [], "bot_paused": false })));
    };

    let messages = db::messages(&app.db, convo_id).await.unwrap_or_default();
    let mut out = Vec::new();
    for m in messages.iter().filter(|m| m.role == "customer" || m.role == "assistant") {
        let vehicles = db::vehicles_by_ids(&app.db, dealer_id, &m.vehicle_ids)
            .await
            .unwrap_or_default();
        out.push(json!({
            "role": m.role, "text": m.content,
            "vehicles": vehicle_cards(&app, &vehicles).await,
        }));
    }
    (StatusCode::OK, Json(json!({ "messages": out, "bot_paused": paused })))
}

/// This conversation's id, if the session has one. Shared by the side panels.
async fn session_convo(app: &App, session_id: &str) -> Option<uuid::Uuid> {
    sqlx::query_scalar(
        "select c.id from conversations c
         join customer_identities i on i.customer_id = c.customer_id
         where i.channel = 'web' and i.external_id = $1
         order by c.started_at desc limit 1",
    )
    .bind(session_id)
    .fetch_optional(&app.db)
    .await
    .ok()
    .flatten()
}

/// Two weeks of open times, grouped by the dealer's local date, plus the
/// appointments this session already holds. Same free_slots the AI's
/// get_available_slots tool reads, so the panel and the bot never disagree.
async fn calendar(State(app): State<App>, Query(q): Query<HistoryQ>) -> impl IntoResponse {
    if !valid_session(&q.session_id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad session id" })));
    }
    let result = async {
        let settings = db::settings(&app.db, db::default_dealer(&app.db).await?).await?;
        let tz = settings.tz();
        let today = chrono::Utc::now().with_timezone(&tz).date_naive();
        let slots = LocalCalendar::new(app.db.clone())
            .free_slots(&settings, today, today + chrono::Duration::days(13), "test_drive", 500)
            .await?;
        // One shared calendar: every confirmed appointment blocks the time,
        // whatever its kind. Other customers' bookings show only as "taken".
        let convo = session_convo(&app, &q.session_id).await;
        let booked: Vec<(chrono::DateTime<chrono::Utc>, String, String, bool)> = sqlx::query_as(
            "select lower(a.slot), a.kind, r.name, coalesce(l.conversation_id = $2, false)
             from appointments a join leads l on l.id = a.lead_id join resources r on r.id = a.resource_id
             where a.dealer_id = $1 and a.status = 'confirmed'
               and upper(a.slot) > now() and lower(a.slot) < now() + interval '14 days'
             order by 1",
        )
        .bind(settings.dealer_id)
        .bind(convo)
        .fetch_all(&app.db)
        .await?;
        let fmt = |t: chrono::DateTime<chrono::Utc>| {
            let l = t.with_timezone(&tz);
            (l.format("%Y-%m-%d").to_string(), l.format("%-I:%M %p").to_string())
        };
        let slots: Vec<_> = slots
            .iter()
            .map(|s| {
                let (date, time) = fmt(s.start);
                json!({ "date": date, "time": time, "local": s.local })
            })
            .collect();
        let booked: Vec<_> = booked
            .into_iter()
            .map(|(t, kind, with, mine)| {
                let (date, time) = fmt(t);
                if mine {
                    json!({ "date": date, "time": time, "kind": kind, "with": with, "mine": true })
                } else {
                    json!({ "date": date, "time": time, "mine": false })
                }
            })
            .collect();
        anyhow::Ok(json!({ "today": today.to_string(), "slots": slots, "booked": booked }))
    }
    .await;
    match result {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err(e) => {
            tracing::error!("calendar failed: {e:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "calendar unavailable" })))
        }
    }
}

/// A quick read of the lead for the salesperson: how interested, how specific,
/// in the model's own words. Advisory only - it never leaves this page, so it
/// skips the evidence checks the emailed summary goes through.
async fn profile(State(app): State<App>, Query(q): Query<HistoryQ>) -> impl IntoResponse {
    if !valid_session(&q.session_id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad session id" })));
    }
    let Some(convo) = session_convo(&app, &q.session_id).await else {
        return (StatusCode::OK, Json(json!({ "profile": null })));
    };
    let result = async {
        let msgs = db::messages(&app.db, convo).await?;
        if !msgs.iter().any(|m| m.role == "customer") || !llm::Client::configured() {
            return anyhow::Ok(json!({ "profile": null }));
        }
        let text: String = msgs
            .iter()
            .filter(|m| m.role == "customer" || m.role == "assistant")
            .map(|m| format!("{}: {}\n", if m.role == "customer" { "Customer" } else { "Assistant" }, m.content))
            .collect();
        let schema = json!({
            "type": "object", "additionalProperties": false,
            "required": ["headline", "language", "interest", "specificity", "wants", "checks", "notes"],
            "properties": {
                "headline": { "type": "string" },
                "interest": { "type": "integer" },
                "specificity": { "type": "integer" },
                "language": { "type": "string", "enum": ["English", "Spanish"] },
                "wants": { "type": "array", "items": { "type": "string" } },
                "checks": {
                    "type": "object", "additionalProperties": false,
                    "required": ["gave_name", "gave_contact", "gave_budget", "wants_financing", "has_trade_in", "wants_test_drive", "buying_soon"],
                    "properties": {
                        "gave_name": { "type": "boolean" }, "gave_contact": { "type": "boolean" },
                        "gave_budget": { "type": "boolean" }, "wants_financing": { "type": "boolean" },
                        "has_trade_in": { "type": "boolean" }, "wants_test_drive": { "type": "boolean" },
                        "buying_soon": { "type": "boolean" }
                    }
                },
                "notes": { "type": "string" }
            }
        });
        let system = "You profile car-dealership leads for a salesperson. From the chat, return: \
            headline (under 10 words, who this buyer is), \
            interest 1-5 (1 browsing, 3 comparing, 5 ready to buy or book), \
            specificity 1-5 (1 vague like 'a car', 5 exact model, budget, timeline), \
            language (English or Spanish, whichever the customer writes in), \
            wants (up to 4 short facts the customer actually stated), \
            checks (true only if the customer clearly said so, otherwise false), \
            notes (one plain sentence: what to do next with them). Only use what the customer said. Reply with that JSON object only.";
        let v = app
            .llm
            .complete_json(system, &[llm::Message::user_text(text)], &schema, 400)
            .await?;
        anyhow::Ok(json!({ "profile": v }))
    }
    .await;
    match result {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err(e) => {
            tracing::error!("profile failed: {e:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "profile unavailable" })))
        }
    }
}

/// Mail this conversation caused, straight from the outbox. Outbox rows carry
/// no conversation id, so they are matched through their idempotency keys,
/// which embed the conversation, lead or appointment id.
async fn emails(State(app): State<App>, Query(q): Query<HistoryQ>) -> impl IntoResponse {
    if !valid_session(&q.session_id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad session id" })));
    }
    let Some(convo) = session_convo(&app, &q.session_id).await else {
        return (StatusCode::OK, Json(json!({ "emails": [] })));
    };
    let rows: Vec<(String, String, serde_json::Value, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "select o.kind, o.status, o.payload, o.created_at from outbox o
         where split_part(o.idempotency_key, ':', 2) in (
             select $1::text
             union select l.id::text from leads l where l.conversation_id = $1
             union select a.id::text from appointments a join leads l on l.id = a.lead_id
                   where l.conversation_id = $1)
         order by o.created_at desc",
    )
    .bind(convo)
    .fetch_all(&app.db)
    .await
    .unwrap_or_default();
    let out: Vec<_> = rows
        .into_iter()
        .map(|(kind, status, p, at)| {
            json!({
                "kind": kind, "status": status, "at": at,
                "to": p["to"], "subject": p["subject"], "text": p["text"],
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({ "emails": out })))
}

async fn health(State(app): State<App>) -> impl IntoResponse {
    let db_ok = sqlx::query("select 1").execute(&app.db).await.is_ok();
    Json(json!({
        "db": db_ok,
        "llm_configured": llm::Client::configured(),
        "model": app.llm.model,
        "mailer": app.mailer.describe(),
    }))
}

async fn index() -> impl IntoResponse {
    match std::fs::read_to_string(automotrix::path("web/index.html")) {
        Ok(html) => Html(html).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "web/index.html missing").into_response(),
    }
}

fn lan_addresses() -> Vec<String> {
    // Resolve the address the OS would use to reach the outside world; no
    // packet is sent, UDP connect just picks a route.
    let mut out = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = sock.local_addr() {
                out.push(addr.ip().to_string());
            }
        }
    }
    if let Ok(h) = std::env::var("PUBLIC_HOSTNAME") {
        if !h.trim().is_empty() {
            out.push(h);
        }
    }
    out
}

/// Self-signed, kept on disk so the browser only warns once. Regenerated if the
/// machine's LAN address changes, since a cert for the old IP would not match.
fn ensure_cert() -> Result<(PathBuf, PathBuf)> {
    let dir = automotrix::path("certs");
    std::fs::create_dir_all(&dir)?;
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    let names_file = dir.join("names.txt");
    let names = lan_addresses();
    let wanted = names.join(",");
    let current = std::fs::read_to_string(&names_file).unwrap_or_default();

    if !(cert.exists() && key.exists() && current == wanted) {
        let generated = rcgen::generate_simple_self_signed(names.clone())
            .context("could not generate a TLS certificate")?;
        std::fs::write(&cert, generated.cert.pem())?;
        std::fs::write(&key, generated.key_pair.serialize_pem())?;
        std::fs::write(&names_file, &wanted)?;
        tracing::info!("generated a self-signed certificate for {wanted}");
    }
    Ok((cert, key))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn,tower_http=warn".into()),
        )
        .init();

    // sqlx and axum-server both pull rustls; pin one crypto backend explicitly
    // so the two do not fight over the process default
    let _ = rustls::crypto::ring::default_provider().install_default();

    automotrix::load_env();
    let db = db::connect(&automotrix::database_url()?).await?;

    let llm = llm::Client::from_env().unwrap_or_else(|_| {
        tracing::warn!("ANTHROPIC_API_KEY not set - the chat will answer with a setup message until it is");
        llm::Client::new(String::new(), None)
    });

    let mut env = minijinja::Environment::new();
    env.set_loader(minijinja::path_loader(automotrix::path("crates/automotrix/templates")));
    env.set_auto_escape_callback(|name| {
        if name.ends_with(".html") {
            minijinja::AutoEscape::Html
        } else {
            minijinja::AutoEscape::None
        }
    });

    let app = App {
        db,
        llm,
        mailer: email::from_env(),
        templates: Arc::new(env),
        summary_schema: Arc::new(summary::load_schema("dealership")?),
    };
    tracing::info!("mail transport: {}", app.mailer.describe());

    engine::spawn_worker(app.clone());

    let router = Router::new()
        .route("/", get(index))
        .route("/api/chat", post(chat))
        .route("/api/history", get(history))
        .route("/api/calendar", get(calendar))
        .route("/api/emails", get(emails))
        .route("/api/profile", get(profile))
        .route("/api/health", get(health))
        // vehicle photos, straight from the repo; they are CC-licensed and
        // their credits are in vehicle_photos
        .nest_service("/cars", ServeDir::new(automotrix::path("cars")))
        .with_state(app);

    let port: u16 = std::env::var("HTTPS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8443);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let (cert, key) = ensure_cert()?;
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;

    for host in lan_addresses() {
        tracing::info!("chat: https://{host}:{port}");
    }
    axum_server::bind_rustls(addr, tls)
        .serve(router.into_make_service())
        .await?;
    Ok(())
}
