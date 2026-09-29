//! WHAT: takes the demo database from nothing to loaded and verified, in one go.
//! WHY:  a demo you cannot rebuild is a demo you are afraid to touch. This throws
//!       the volume away every time, so the result never depends on what happened
//!       in a previous run.
//! HOW:  down -v (drops the named volume) -> up -> wait for the healthcheck ->
//!       sqlx migrate run -> load -> verify. Any step failing stops the run.
//!
//! Pass --rebuild-seed to regenerate the synthetic inventory and re-embed the
//! corpus before loading (the first run downloads the ~440MB embedding model into
//! .fastembed_cache/). Without it the committed seed/*.json and seed/*.ndjson are
//! used as-is.
//!
//!   cargo run -p seed --bin reset_db [-- --rebuild-seed]

use anyhow::{bail, Context, Result};
use std::process::Command;
use std::{thread, time::Duration};

fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(seed::repo_root())
        .status()
        .with_context(|| format!("no se pudo ejecutar {program}"))?;
    if !status.success() {
        bail!("fallo: {program} {}", args.join(" "));
    }
    Ok(())
}

fn db_health() -> String {
    Command::new("docker")
        .args(["inspect", "-f", "{{.State.Health.Status}}", "automotrix-db"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn main() -> Result<()> {
    let root = seed::repo_root();
    // docker compose and sqlx both read .env from the working directory
    dotenvy::from_path(root.join(".env")).context("falta .env - copia .env.example y ajustalo")?;

    if std::env::args().any(|a| a == "--rebuild-seed") {
        println!("==> regenerando inventario y corpus");
        run("cargo", &["run", "--release", "--quiet", "-p", "datagen", "--bin", "gen_inventory"])?;
        run("cargo", &["run", "--release", "--quiet", "-p", "datagen", "--bin", "build_corpus"])?;
    }

    println!("==> compilando el loader");
    run("cargo", &["build", "--release", "--quiet", "-p", "seed"])?;

    println!("==> borrando contenedor y volumen");
    run("docker", &["compose", "down", "-v", "--remove-orphans"])?;

    println!("==> levantando Postgres y Mailpit");
    run("docker", &["compose", "up", "-d", "db", "mailpit"])?;

    print!("==> esperando healthcheck");
    let mut healthy = false;
    for _ in 0..60 {
        if db_health() == "healthy" {
            healthy = true;
            break;
        }
        print!(".");
        thread::sleep(Duration::from_secs(1));
    }
    if !healthy {
        println!(" la base nunca quedo healthy");
        let _ = run("docker", &["compose", "logs", "--tail=40", "db"]);
        bail!("la base nunca quedo healthy");
    }
    println!(" listo");

    println!("==> migraciones");
    run("sqlx", &["migrate", "run", "--source", "migrations"])?;

    println!("==> carga");
    run("./target/release/load", &[])?;

    println!("==> verificacion");
    run("./target/release/verify", &[])?;

    println!("==> base lista en {}", std::env::var("DATABASE_URL").unwrap_or_default());
    Ok(())
}
