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
