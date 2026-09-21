//! WHAT: proves the loaded database is complete and that vector search works.
//! WHY:  a silently half-loaded RAG database still answers questions - just
//!       wrongly. These checks fail loudly instead, and the binary exits
//!       non-zero so the reset script stops on them.
//! HOW:  counts rows against the source files, asserts the required columns hold
//!       no NULLs, then runs a self-retrieval test: searching with a chunk's own
//!       stored vector must return that same chunk first. Anything less means
//!       the text and the vectors have drifted apart.

use anyhow::{Context, Result};
use pgvector::Vector;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use seed::{database_url, path};
use serde::Deserialize;
use sqlx::{PgPool, Row};
use std::fs;
use uuid::Uuid;

#[derive(Deserialize)]
struct Inventory {
    vehicles: Vec<serde_json::Value>,
    internal: Vec<serde_json::Value>,
    photos: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Doc {
    chunks: Vec<serde_json::Value>,
}

struct Report {
    failures: Vec<String>,
}

impl Report {
    fn check(&mut self, label: &str, ok: bool, detail: &str) {
        let tag = if ok { "OK " } else { "FAIL" };
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(" - {detail}")
        };
        println!("  [{tag}] {label}{suffix}");
        if !ok {
            self.failures.push(label.to_string());
        }
    }
}

/// sqlx 0.9 refuses SQL built at runtime, which is the right default. Every
/// statement here is a compile-time literal.
async fn scalar(pool: &PgPool, sql: &'static str) -> Result<i64> {
    let r: (i64,) = sqlx::query_as(sql).fetch_one(pool).await?;
    Ok(r.0)
}

#[tokio::main]
async fn main() -> Result<()> {
    let pool = PgPool::connect(&database_url()?).await?;
    let mut rep = Report { failures: vec![] };

    let inv: Inventory = serde_json::from_str(&fs::read_to_string(path("seed/inventory.json"))?)?;
    let corpus_txt = fs::read_to_string(path("seed/corpus.ndjson"))?;
    let docs: Vec<Doc> = corpus_txt
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let src_chunks: usize = docs.iter().map(|d| d.chunks.len()).sum();
    let src_specs = fs::read_dir(path("cars"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("specs.json").exists())
        .count();

    println!("\n1. CONTEO DE FILAS vs FUENTE");
    for (table, sql, expected) in [
        ("dealers", "select count(*) from dealers", 1usize),
        ("vehicles", "select count(*) from vehicles", inv.vehicles.len()),
        ("vehicle_internal", "select count(*) from vehicle_internal", inv.internal.len()),
        ("vehicle_photos", "select count(*) from vehicle_photos", inv.photos.len()),
        ("trim_specs", "select count(*) from trim_specs", src_specs),
        ("documents", "select count(*) from documents", docs.len()),
        ("doc_chunks", "select count(*) from doc_chunks", src_chunks),
    ] {
        let got = scalar(&pool, sql).await? as usize;
        rep.check(
            &format!("{table}: {got}"),
            got == expected,
            &format!("fuente {expected}"),
        );
    }

    println!("\n2. INTEGRIDAD");
    let nulls = scalar(&pool,
        "select count(*) from vehicles where vin is null or year is null
         or make is null or model is null or mileage is null
         or list_price_cents is null").await?;
    rep.check("vehicles sin NULL en columnas obligatorias", nulls == 0,
              &format!("{nulls} filas malas"));

    let bad_vin = scalar(&pool,
        r"select count(*) from vehicles where vin !~ '^[A-HJ-NPR-Z0-9]{17}$'").await?;
    rep.check("VIN con formato valido", bad_vin == 0, &format!("{bad_vin} malos"));

    let dup = scalar(&pool,
        "select count(*) from (select vin from vehicles group by vin having count(*) > 1) t")
        .await?;
    rep.check("VIN sin duplicados", dup == 0, &format!("{dup} repetidos"));

    let row = sqlx::query(
        "select count(distinct vector_dims(embedding))::bigint as kinds,
                min(vector_dims(embedding))::bigint as dim from doc_chunks")
        .fetch_one(&pool).await?;
    let kinds: i64 = row.get("kinds");
    let dim: Option<i64> = row.get("dim");
    rep.check("todos los vectores con la misma dimension", kinds == 1,
              &format!("dim={}", dim.unwrap_or(0)));

    let models: Vec<(String,)> =
        sqlx::query_as("select distinct embedding_model from doc_chunks")
            .fetch_all(&pool).await?;
    rep.check("un solo embedding_model", models.len() == 1,
              models.first().map(|m| m.0.as_str()).unwrap_or("ninguno"));

    let naked = scalar(&pool,
        "select count(*) from doc_chunks where risk = 'high' and disclaimer is null").await?;
    rep.check("ningun chunk high-risk sin disclaimer", naked == 0,
              &format!("{naked} desnudos"));

    let prim = scalar(&pool,
        "select count(*) from (select vehicle_id from vehicle_photos where is_primary
         group by vehicle_id having count(*) <> 1) t").await?;
    rep.check("exactamente una foto primaria por vehiculo", prim == 0, "");

    println!("\n3. SELF-RETRIEVAL (20 chunks al azar, top-1 debe ser el mismo)");
    let ids: Vec<(Uuid,)> =
        sqlx::query_as("select id from doc_chunks where audience = 'customer'")
            .fetch_all(&pool).await?;
    let mut ids: Vec<Uuid> = ids.into_iter().map(|(i,)| i).collect();
    // fixed seed so a failure is reproducible
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    ids.shuffle(&mut rng);
    let sample: Vec<Uuid> = ids.into_iter().take(20).collect();

    let mut hits = 0;
    let mut misses: Vec<(Uuid, Uuid)> = vec![];
    for id in &sample {
        let v: (Vector,) = sqlx::query_as("select embedding from doc_chunks where id = $1")
            .bind(id).fetch_one(&pool).await?;
        let top: (Uuid,) = sqlx::query_as(
            "select id from doc_chunks where audience = 'customer'
             order by embedding <=> $1 limit 1")
            .bind(&v.0).fetch_one(&pool).await?;
        if top.0 == *id { hits += 1 } else { misses.push((*id, top.0)) }
    }
    rep.check(&format!("self-retrieval {hits}/{}", sample.len()),
              hits == sample.len(),
              &if misses.is_empty() { String::new() }
               else { format!("fallos: {:?}", &misses[..misses.len().min(3)]) });
    if !misses.is_empty() {
        let d = scalar(&pool,
            "select count(*) from (select content_hash from doc_chunks
             group by content_hash having count(*) > 1) t").await?;
        println!("       chunks con content_hash duplicado: {d}");
    }

    let dealer: (Uuid,) = sqlx::query_as("select id from dealers limit 1")
        .fetch_one(&pool).await.context("no hay dealer cargado")?;

    println!("\n4. CONSULTAS DE INVENTARIO");
    println!("  'SUVs disponibles bajo $25,000 con menos de 60,000 millas'");
    let rows = sqlx::query(
        "select stock_number, vehicle, mileage, list_price_cents
         from search_inventory($1, p_body_type => 'suv',
              p_max_price_cents => 2500000, p_max_mileage => 60000)")
        .bind(dealer.0).fetch_all(&pool).await?;
    for r in &rows {
        let price: i64 = r.get("list_price_cents");
        let miles: i32 = r.get("mileage");
        println!("     {:10} {:36} {:>7} mi  ${:>9}",
                 r.get::<String, _>("stock_number"),
                 r.get::<String, _>("vehicle"),
                 miles, price / 100);
    }
    println!("     -> {} resultados", rows.len());

    println!("\n  'pickups 2019 o mas nuevas con 4x4/4WD'");
    let rows = sqlx::query(
        "select stock_number, vehicle, drivetrain, list_price_cents
         from search_inventory($1, p_body_type => 'truck',
              p_year_min => 2019::smallint, p_drivetrain => '4WD')")
        .bind(dealer.0).fetch_all(&pool).await?;
    for r in &rows {
        let price: i64 = r.get("list_price_cents");
        println!("     {:10} {:36} {:18} ${:>9}",
                 r.get::<String, _>("stock_number"),
                 r.get::<String, _>("vehicle"),
                 r.get::<Option<String>, _>("drivetrain").unwrap_or_default(),
                 price / 100);
    }
    println!("     -> {} resultados", rows.len());

    println!("\n5. v_inventory (10 filas, como la pantalla de un DMS)");
    println!("     {:10} {:34} {:5} {:>7} {:>12} {:>4} {:>3} STATUS",
             "STOCK", "VEHICLE", "COND", "MILES", "PRICE", "DAYS", "PH");
    for r in sqlx::query("select * from v_inventory limit 10").fetch_all(&pool).await? {
        let veh: String = r.get("vehicle");
        println!("     {:10} {:34} {:5} {:>7} {:>12} {:>4} {:>3} {}",
                 r.get::<String, _>("stock_number"),
                 veh.chars().take(34).collect::<String>(),
                 r.get::<String, _>("condition"),
                 r.get::<i32, _>("mileage"),
                 r.get::<String, _>("list_price"),
                 r.get::<Option<i32>, _>("days_in_stock").unwrap_or(0),
                 r.get::<i64, _>("photo_count"),
                 r.get::<String, _>("status"));
    }

    if rep.failures.is_empty() {
        println!("\nTODO OK");
        Ok(())
    } else {
        println!("\nFALLOS: {:?}", rep.failures);
        std::process::exit(1);
    }
}
