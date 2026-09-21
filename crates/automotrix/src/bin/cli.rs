//! WHAT: talk to the bot from a terminal.
//! WHY:  the fastest loop for trying a prompt change or a tool, with no browser
//!       and no TLS in the way. Also the test harness the brief asks for.
//! HOW:  each run is one conversation on the 'cli' channel, keyed by a session
//!       id you can pass to resume it. Type a message, get the reply, repeat.
//!       `/worker` runs one outbox pass so you can see the emails get built.
//!
//!   cargo run --bin cli
//!   cargo run --bin cli -- --session my-test-1

use anyhow::Result;
use automotrix::{db, email, engine, llm, summary, App};
use std::io::{BufRead, Write};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args: Vec<String> = std::env::args().collect();
    let session = args
        .iter()
        .position(|a| a == "--session")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| format!("cli-{}", uuid::Uuid::now_v7().simple()));

    let db = db::connect(&automotrix::database_url()?).await?;
    let mut env = minijinja::Environment::new();
    env.set_loader(minijinja::path_loader(automotrix::path("crates/automotrix/templates")));
    let app = App {
        db,
        llm: llm::Client::from_env()?,
        mailer: email::from_env(),
        templates: Arc::new(env),
        summary_schema: Arc::new(summary::load_schema("dealership")?),
    };

    let dealer_id = db::default_dealer(&app.db).await?;
    let convo = db::conversation_for_identity(&app.db, dealer_id, "cli", &session).await?;
    println!("Automotrix CLI - session {session}");
    println!("model {} | mail {} | /worker to flush email, /quit to exit\n", app.llm.model, app.mailer.describe());

    let stdin = std::io::stdin();
    loop {
        print!("you> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        match line {
            "" => continue,
            "/quit" | "/exit" => break,
            "/worker" => {
                let leads = engine::deliver_pending_leads(&app).await?;
                let sent = automotrix::outbox::tick(&app.db, app.mailer.as_ref()).await?;
                println!("[worker] {leads} lead(s) assembled, {sent} email(s) processed\n");
                continue;
            }
            _ => {}
        }
        match engine::turn(&app, &convo, line).await {
            Ok(r) => {
                println!("\nbot> {}\n", r.text);
                for v in &r.vehicles {
                    println!("     [{}] {} - {} - {} mi", v.stock_number, v.label(), v.price(), v.mileage);
                }
                if !r.vehicles.is_empty() {
                    println!();
                }
                if r.handoff {
                    println!("[handed off to a salesperson - the bot is now paused]\n");
                }
            }
            Err(e) => println!("\n[error] {e:#}\n"),
        }
    }
    Ok(())
}
