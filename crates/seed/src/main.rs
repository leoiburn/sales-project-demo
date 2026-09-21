//! WHAT: loads the three datasets into Postgres - inventory (vehicles, internal
//!       costs, photos), model specs, and the RAG corpus with its vectors.
//! WHY:  the bot answers inventory questions with typed SQL and policy questions
//!       with vector search. Both halves have to be complete and correctly typed
//!       or the bot quotes a car that is not on the lot.
//! HOW:  every dataset goes through the same three steps.
//!         1. COPY the raw file into a staging table whose columns are ALL text
//!         2. normalize and validate inside SQL, stamping a reject_reason on
//!            any row that fails - nothing is dropped silently
//!         3. INSERT ... SELECT with explicit casts into the real table
//!       Staging tables are temp with ON COMMIT DROP, so they clean themselves
//!       up when the dataset's transaction ends.
//!
//!       Ids are uuid v5 of a natural key, so a second run computes the same
//!       ids and the upserts become no-ops. That is the whole idempotency story.
//!
//!       Application validation cannot catch everything: a stock number that
//!       moved from one VIN to another only collides against rows already in
//!       the table. So when a bulk insert trips a constraint, it is retried one
//!       row at a time inside savepoints and the database's own error becomes
//!       the reject reason.

use anyhow::{Context, Result};
use seed::{copy_line, database_url, path, uid, EMBEDDING_DIM};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::AssertSqlSafe;
use sqlx::{Executor, PgPool, Postgres, Transaction};
use std::collections::HashMap;
use std::fs;
use uuid::Uuid;

// ---------------------------------------------------------------- source files

#[derive(Deserialize)]
struct Inventory {
    dealer: Dealer,
    vehicles: Vec<Vehicle>,
    internal: Vec<Internal>,
    photos: Vec<Photo>,
}

#[derive(Deserialize)]
struct Dealer {
    name: String,
    phone: String,
    address: String,
    timezone: String,
}

/// Money and mileage arrive as the formatted strings a real DMS export
/// produces ("$28,800", "45,021"); SQL normalizes them.
#[derive(Deserialize)]
struct Vehicle {
    unit_id: String,
    catalog_id: String,
    stock_number: String,
    vin: String,
    condition: String,
    status: String,
    year: i32,
    make: String,
    model: String,
    trim_level: Option<String>,
    body_type: String,
    doors: Option<i32>,
    exterior_color: Option<String>,
    interior_color: Option<String>,
    engine: Option<String>,
    transmission: Option<String>,
    drivetrain: Option<String>,
    fuel_type: Option<String>,
    mpg_city: Option<i32>,
    mpg_hwy: Option<i32>,
    mileage: String,
    list_price: String,
    msrp: String,
    features: Vec<String>,
    description: Option<String>,
    date_in_stock: String,
}

#[derive(Deserialize)]
struct Internal {
    unit_id: String,
    acquisition_cost: String,
    recon_cost: String,
    acquired_from: String,
    internal_notes: String,
}

#[derive(Deserialize)]
struct Photo {
    unit_id: String,
    position: i32,
    storage_path: String,
    is_primary: bool,
}

#[derive(Deserialize)]
struct PhotoCredit {
    file: String,
    source: Option<String>,
    license: Option<String>,
    author: Option<String>,
}

#[derive(Deserialize)]
struct Document {
    doc_key: String,
    title: String,
    kind: String,
    source_path: String,
    body: String,
    content_hash: String,
    chunks: Vec<Chunk>,
}

#[derive(Deserialize)]
struct Chunk {
    chunk_index: i32,
    heading: String,
    content: String,
    token_count: i32,
    embedding: Vec<f32>,
    embedding_model: String,
    audience: String,
    risk: String,
    disclaimer: Option<String>,
    metadata: serde_json::Value,
    content_hash: String,
}

// ------------------------------------------------------------------- rejects

struct Rejects {
    rows: HashMap<String, Vec<(String, String, String)>>,
}

impl Rejects {
    fn new() -> Self {
        Self { rows: HashMap::new() }
    }

    fn add(&mut self, dataset: &str, key: &str, reason: &str, row: &str) {
        self.rows.entry(dataset.to_string()).or_default().push((
            key.to_string(),
            reason.to_string(),
            row.chars().take(1000).collect(),
        ));
    }

    /// One CSV per dataset. Files for datasets with no rejects are removed, so a
    /// stale file from an earlier run never reads as a current failure.
    fn write(&self) -> Result<()> {
        let dir = path("seed/rejects");
        fs::create_dir_all(&dir)?;
        for dataset in ["vehicles", "vehicle_internal", "photos", "trim_specs",
                        "documents", "doc_chunks"] {
            let file = dir.join(format!("{dataset}.csv"));
            match self.rows.get(dataset) {
                Some(rows) => {
                    let mut w = csv::Writer::from_path(&file)?;
                    w.write_record(["key", "reason", "row"])?;
                    for (k, r, raw) in rows {
                        w.write_record([k, r, raw])?;
                    }
                    w.flush()?;
                    println!("  rechazados {dataset}: {} -> {}", rows.len(), file.display());
                }
                None => {
                    let _ = fs::remove_file(&file);
                }
            }
        }
        if self.rows.is_empty() {
            println!("  rechazados: 0 en todos los datasets");
        }
        Ok(())
    }
}

// ------------------------------------------------------------------- helpers

/// Runs a bulk `INSERT ... SELECT ... FROM <staging>` inside a savepoint. If a
/// constraint fires, retries the same statement per row (the SQL carries a
/// `and s.id = $2` filter) so the offending row is named instead of taking the
/// whole dataset down with it.
/// The two statements are assembled from compile-time literals only - no value
/// from any data file reaches them, every real value is a bind parameter - so
/// AssertSqlSafe is accurate rather than a way around sqlx 0.9's injection guard.
async fn insert_or_isolate<'q>(
    tx: &mut Transaction<'_, Postgres>,
    bulk_sql: &'q str,
    row_sql: &'q str,
    dealer_id: Uuid,
    ids: &[(Uuid, String)],
    dataset: &str,
    rej: &mut Rejects,
) -> Result<u64> {
    tx.execute("savepoint bulk_ins").await?;
    match sqlx::query(AssertSqlSafe(bulk_sql)).bind(dealer_id).execute(&mut **tx).await {
        Ok(r) => {
            tx.execute("release savepoint bulk_ins").await?;
            return Ok(r.rows_affected());
        }
        Err(e) => {
            eprintln!("  lote {dataset} rechazado por la base ({}); reintentando fila por fila",
                      short_err(&e));
            tx.execute("rollback to savepoint bulk_ins").await?;
            tx.execute("release savepoint bulk_ins").await?;
        }
    }

    let mut ok = 0u64;
    for (id, key) in ids {
        tx.execute("savepoint row_ins").await?;
        match sqlx::query(AssertSqlSafe(row_sql))
            .bind(dealer_id)
            .bind(id)
            .execute(&mut **tx)
            .await
        {
            Ok(r) => {
                tx.execute("release savepoint row_ins").await?;
                ok += r.rows_affected();
            }
            Err(e) => {
                tx.execute("rollback to savepoint row_ins").await?;
                tx.execute("release savepoint row_ins").await?;
                rej.add(dataset, key, &short_err(&e), "");
            }
        }
    }
    Ok(ok)
}

fn short_err(e: &sqlx::Error) -> String {
    let s = e.to_string();
    s.lines().next().unwrap_or(&s).trim().to_string()
}

/// Reads the reject_reason column the validation SQL stamped on staging rows.
async fn collect_rejects(
    tx: &mut Transaction<'_, Postgres>,
    dataset: &str,
    rej: &mut Rejects,
) -> Result<()> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "select coalesce(stock_number, '?'), reject_reason, to_jsonb(s)::text
         from stg_vehicles s where reject_reason is not null",
    )
    .fetch_all(&mut **tx)
    .await?;
    for (k, r, row) in rows {
        rej.add(dataset, &k, &r, &row);
    }
    Ok(())
}

// ----------------------------------------------------------------- inventory

async fn load_dealer(tx: &mut Transaction<'_, Postgres>, d: &Dealer) -> Result<Uuid> {
    let id = uid(&["dealer", &d.name]);
    sqlx::query(
        "insert into dealers (id, name, phone, address, timezone)
         values ($1, $2, $3, $4, $5)
         on conflict (id) do update set
             name = excluded.name, phone = excluded.phone,
             address = excluded.address, timezone = excluded.timezone",
    )
    .bind(id)
    .bind(&d.name)
    .bind(&d.phone)
    .bind(&d.address)
    .bind(&d.timezone)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

async fn load_vehicles(
    tx: &mut Transaction<'_, Postgres>,
    dealer_id: Uuid,
    inv: &Inventory,
    rej: &mut Rejects,
) -> Result<(HashMap<String, Uuid>, u64)> {
    tx.execute(
        "create temp table stg_vehicles (
            id text, unit_id text, stock_number text, vin text, condition text,
            status text, year text, make text, model text, trim_level text,
            body_type text, doors text, exterior_color text, interior_color text,
            engine text, transmission text, drivetrain text, fuel_type text,
            mpg_city text, mpg_hwy text, mileage text, list_price text, msrp text,
            features text, description text, date_in_stock text
         ) on commit drop",
    )
    .await?;

    let mut ids = Vec::new();
    let mut buf = String::new();
    for v in &inv.vehicles {
        let vin = v.vin.trim().to_uppercase();
        let id = uid(&["vehicle", &dealer_id.to_string(), &vin]);
        ids.push((id, v.unit_id.clone()));
        buf.push_str(&copy_line(&[
            Some(id.to_string()),
            Some(v.unit_id.clone()),
            Some(v.stock_number.clone()),
            Some(v.vin.clone()),
            Some(v.condition.clone()),
            Some(v.status.clone()),
            Some(v.year.to_string()),
            Some(v.make.clone()),
            Some(v.model.clone()),
            v.trim_level.clone(),
            Some(v.body_type.clone()),
            v.doors.map(|d| d.to_string()),
            v.exterior_color.clone(),
            v.interior_color.clone(),
            v.engine.clone(),
            v.transmission.clone(),
            v.drivetrain.clone(),
            v.fuel_type.clone(),
            v.mpg_city.map(|d| d.to_string()),
            v.mpg_hwy.map(|d| d.to_string()),
            Some(v.mileage.clone()),
            Some(v.list_price.clone()),
            Some(v.msrp.clone()),
            Some(serde_json::to_string(&v.features)?),
            v.description.clone(),
            Some(v.date_in_stock.clone()),
        ]));
    }
    let mut copy = tx.copy_in_raw("copy stg_vehicles from stdin").await?;
    copy.send(buf.as_bytes()).await?;
    copy.finish().await?;

    // Normalize into typed columns. Stripping everything but digits turns
    // "$28,800" into 28800 and "45,021" into 45021; a field with no digits at
    // all collapses to NULL, which the validation below then rejects.
    tx.execute(
        "alter table stg_vehicles
            add column n_vin text, add column n_year int, add column n_mileage int,
            add column n_price bigint, add column n_msrp bigint,
            add column reject_reason text",
    )
    .await?;
    tx.execute(
        "update stg_vehicles set
            n_vin = upper(btrim(vin)),
            n_year = nullif(regexp_replace(coalesce(year,''), '[^0-9]', '', 'g'), '')::int,
            n_mileage = nullif(regexp_replace(coalesce(mileage,''), '[^0-9-]', '', 'g'), '')::int,
            n_price = round(nullif(regexp_replace(coalesce(list_price,''), '[^0-9.]', '', 'g'), '')::numeric * 100)::bigint,
            n_msrp = round(nullif(regexp_replace(coalesce(msrp,''), '[^0-9.]', '', 'g'), '')::numeric * 100)::bigint",
    )
    .await?;

    tx.execute(
        "update stg_vehicles set reject_reason = case
            when n_vin !~ '^[A-HJ-NPR-Z0-9]{17}$' then 'VIN missing or malformed'
            when btrim(coalesce(make,'')) = '' or btrim(coalesce(model,'')) = ''
                then 'make or model missing'
            when lower(btrim(coalesce(condition,''))) not in ('new','used','cpo')
                then 'unknown condition: ' || coalesce(condition,'(null)')
            when lower(btrim(coalesce(status,''))) not in ('available','pending','sold')
                then 'unknown status: ' || coalesce(status,'(null)')
            when lower(btrim(coalesce(body_type,''))) not in
                 ('sedan','suv','truck','van','coupe','hatchback','wagon','convertible')
                then 'unknown body_type: ' || coalesce(body_type,'(null)')
            when n_year is null or n_year not between 1990 and 2030
                then 'year out of range: ' || coalesce(year,'(null)')
            when n_mileage is null or n_mileage < 0
                then 'mileage missing or negative: ' || coalesce(mileage,'(null)')
            when n_price is null or n_price <= 0
                then 'list price missing or not positive: ' || coalesce(list_price,'(null)')
         end",
    )
    .await?;

    // Two units cannot share a VIN or a stock number. Keep the first, reject
    // the rest, so the source file's own duplicates are reported rather than
    // silently collapsed by the upsert.
    for sql in [
        "update stg_vehicles s set reject_reason = 'duplicate VIN in source'
         from (select ctid, row_number() over (partition by n_vin order by ctid) rn
               from stg_vehicles where reject_reason is null) d
         where d.ctid = s.ctid and d.rn > 1 and s.reject_reason is null",
        "update stg_vehicles s set reject_reason = 'duplicate stock_number in source'
         from (select ctid, row_number() over (partition by stock_number order by ctid) rn
               from stg_vehicles where reject_reason is null) d
         where d.ctid = s.ctid and d.rn > 1 and s.reject_reason is null",
    ] {
        tx.execute(sql).await?;
    }

    let cols = "id, dealer_id, stock_number, vin, condition, status, year, make,
                model, trim_level, body_type, doors, exterior_color, interior_color,
                engine, transmission, drivetrain, fuel_type, mpg_city, mpg_hwy,
                mileage, list_price_cents, msrp_cents, features, description,
                date_in_stock";
    let select = "select s.id::uuid, $1::uuid, s.stock_number, s.n_vin,
                    lower(btrim(s.condition)), lower(btrim(s.status)),
                    s.n_year::smallint, s.make, s.model, nullif(btrim(coalesce(s.trim_level,'')),''),
                    lower(btrim(s.body_type)), nullif(s.doors,'')::smallint,
                    s.exterior_color, s.interior_color, s.engine, s.transmission,
                    s.drivetrain, s.fuel_type, nullif(s.mpg_city,'')::smallint,
                    nullif(s.mpg_hwy,'')::smallint, s.n_mileage, s.n_price, s.n_msrp,
                    array(select jsonb_array_elements_text(s.features::jsonb)),
                    s.description, nullif(s.date_in_stock,'')::date
                  from stg_vehicles s where s.reject_reason is null";
    let upsert = "on conflict (dealer_id, vin) do update set
                    stock_number = excluded.stock_number, condition = excluded.condition,
                    status = excluded.status, trim_level = excluded.trim_level,
                    mileage = excluded.mileage, list_price_cents = excluded.list_price_cents,
                    msrp_cents = excluded.msrp_cents, features = excluded.features,
                    description = excluded.description,
                    date_in_stock = excluded.date_in_stock, updated_at = now()";

    let bulk = format!("insert into vehicles ({cols}) {select} {upsert}");
    let per_row = format!("insert into vehicles ({cols}) {select} and s.id::uuid = $2 {upsert}");

    // Read the id/stock pairs back from staging rather than from the in-memory
    // vector: two source rows sharing a VIN share a uuid, so an id-keyed map
    // would collapse them and label a rejection with the wrong stock number.
    let good: Vec<(Uuid, String)> = sqlx::query_as(
        "select id::uuid, stock_number from stg_vehicles where reject_reason is null
         order by ctid",
    )
    .fetch_all(&mut **tx)
    .await?;

    let n = insert_or_isolate(tx, &bulk, &per_row, dealer_id, &good, "vehicles", rej).await?;
    collect_rejects(tx, "vehicles", rej).await?;

    // unit_id -> vehicle uuid, but only for units that actually landed
    let live: Vec<(Uuid,)> = sqlx::query_as("select id from vehicles where dealer_id = $1")
        .bind(dealer_id)
        .fetch_all(&mut **tx)
        .await?;
    let live: std::collections::HashSet<Uuid> = live.into_iter().map(|(i,)| i).collect();
    let map = ids
        .iter()
        .filter(|(i, _)| live.contains(i))
        .map(|(i, u)| (u.clone(), *i))
        .collect();
    Ok((map, n))
}

async fn load_internal(
    tx: &mut Transaction<'_, Postgres>,
    dealer_id: Uuid,
    inv: &Inventory,
    map: &HashMap<String, Uuid>,
) -> Result<u64> {
    let mut n = 0;
    for i in &inv.internal {
        let Some(vid) = map.get(&i.unit_id) else { continue };
        let r = sqlx::query(
            "insert into vehicle_internal (vehicle_id, dealer_id, acquisition_cost_cents,
                 recon_cost_cents, acquired_from, internal_notes)
             values ($1, $2,
                 round(nullif(regexp_replace($3, '[^0-9.]', '', 'g'), '')::numeric * 100)::bigint,
                 round(nullif(regexp_replace($4, '[^0-9.]', '', 'g'), '')::numeric * 100)::bigint,
                 $5, $6)
             on conflict (vehicle_id) do update set
                 acquisition_cost_cents = excluded.acquisition_cost_cents,
                 recon_cost_cents = excluded.recon_cost_cents,
                 acquired_from = excluded.acquired_from,
                 internal_notes = excluded.internal_notes",
        )
        .bind(vid)
        .bind(dealer_id)
        .bind(&i.acquisition_cost)
        .bind(&i.recon_cost)
        .bind(&i.acquired_from)
        .bind(&i.internal_notes)
        .execute(&mut **tx)
        .await?;
        n += r.rows_affected();
    }
    Ok(n)
}

async fn load_photos(
    tx: &mut Transaction<'_, Postgres>,
    dealer_id: Uuid,
    inv: &Inventory,
    map: &HashMap<String, Uuid>,
    rej: &mut Rejects,
) -> Result<u64> {
    // credits are per car folder, keyed by the path inside it
    let mut credits: HashMap<String, PhotoCredit> = HashMap::new();
    for cat in inv.vehicles.iter().map(|v| &v.catalog_id).collect::<std::collections::HashSet<_>>() {
        let p = path(&format!("cars/{cat}/photo-credits.json"));
        if let Ok(txt) = fs::read_to_string(&p) {
            for c in serde_json::from_str::<Vec<PhotoCredit>>(&txt)? {
                credits.insert(format!("cars/{cat}/{}", c.file), c);
            }
        }
    }

    let mut n = 0;
    for ph in &inv.photos {
        let Some(vid) = map.get(&ph.unit_id) else {
            rej.add("photos", &ph.unit_id, "vehicle was rejected", &ph.storage_path);
            continue;
        };
        if !path(&ph.storage_path).exists() {
            rej.add("photos", &ph.storage_path, "file not found on disk", &ph.storage_path);
            continue;
        }
        let c = credits.get(&ph.storage_path);
        let r = sqlx::query(
            "insert into vehicle_photos (id, vehicle_id, dealer_id, position,
                 storage_path, public_url, is_primary, source_url, author, license)
             values ($1, $2, $3, $4, $5, null, $6, $7, $8, $9)
             on conflict (vehicle_id, position) do update set
                 storage_path = excluded.storage_path, is_primary = excluded.is_primary,
                 source_url = excluded.source_url, author = excluded.author,
                 license = excluded.license",
        )
        .bind(uid(&["photo", &vid.to_string(), &ph.position.to_string()]))
        .bind(vid)
        .bind(dealer_id)
        .bind(ph.position as i16)
        .bind(&ph.storage_path)
        .bind(ph.is_primary)
        .bind(c.and_then(|c| c.source.clone()))
        .bind(c.and_then(|c| c.author.clone()))
        .bind(c.and_then(|c| c.license.clone()))
        .execute(&mut **tx)
        .await?;
        n += r.rows_affected();
    }
    Ok(n)
}

// ---------------------------------------------------------------- trim_specs

async fn load_trim_specs(tx: &mut Transaction<'_, Postgres>, rej: &mut Rejects) -> Result<u64> {
    let mut n = 0;
    let mut dir: Vec<_> = fs::read_dir(path("cars"))?.filter_map(|e| e.ok()).collect();
    dir.sort_by_key(|e| e.path());

    for entry in dir {
        let p = entry.path().join("specs.json");
        if !p.exists() {
            continue;
        }
        let spec: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p)?)?;
        let (year, make, model) = (
            spec["model_year"].as_i64(),
            spec["make"].as_str(),
            spec["model"].as_str(),
        );
        let (Some(year), Some(make), Some(model)) = (year, make, model) else {
            rej.add("trim_specs", &p.display().to_string(),
                    "missing year, make or model", "");
            continue;
        };

        // structured figures only - the prose sections are RAG material
        let mut structured = serde_json::Map::new();
        for k in ["generation", "body_style", "segment", "country_of_origin",
                  "msrp_usd", "typical_used_price_usd", "trims", "powertrains",
                  "dimensions", "capacity", "safety", "warranty", "tech",
                  "colors_exterior", "interior_materials", "charging"] {
            if let Some(v) = spec.get(k) {
                structured.insert(k.to_string(), v.clone());
            }
        }

        let r = sqlx::query(
            "insert into trim_specs (id, year, make, model, trim_level, specs, source)
             values ($1, $2, $3, $4, null, $5, $6)
             on conflict (id) do update set specs = excluded.specs, source = excluded.source",
        )
        .bind(uid(&["trimspec", &year.to_string(), make, model]))
        .bind(year as i16)
        .bind(make)
        .bind(model)
        .bind(serde_json::Value::Object(structured))
        .bind(
            p.strip_prefix(seed::repo_root())
                .unwrap_or(&p)
                .display()
                .to_string(),
        )
        .execute(&mut **tx)
        .await?;
        n += r.rows_affected();
    }
    Ok(n)
}

// -------------------------------------------------------------------- corpus

async fn load_corpus(
    tx: &mut Transaction<'_, Postgres>,
    dealer_id: Uuid,
    rej: &mut Rejects,
) -> Result<(u64, u64)> {
    let text = fs::read_to_string(path("seed/corpus.ndjson"))
        .context("seed/corpus.ndjson missing - run scripts/build_corpus.py")?;

    let mut n_doc = 0;
    let mut n_chunk = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let doc: Document = serde_json::from_str(line)?;
        let doc_id = uid(&["document", &dealer_id.to_string(), &doc.doc_key]);

        sqlx::query(
            "insert into documents (id, dealer_id, doc_key, title, kind, source_path,
                 body, content_hash)
             values ($1, $2, $3, $4, $5, $6, $7, $8)
             on conflict (dealer_id, doc_key) do update set
                 title = excluded.title, kind = excluded.kind,
                 source_path = excluded.source_path, body = excluded.body,
                 content_hash = excluded.content_hash",
        )
        .bind(doc_id)
        .bind(dealer_id)
        .bind(&doc.doc_key)
        .bind(&doc.title)
        .bind(&doc.kind)
        .bind(&doc.source_path)
        .bind(&doc.body)
        .bind(&doc.content_hash)
        .execute(&mut **tx)
        .await?;
        n_doc += 1;

        for c in &doc.chunks {
            let why = if c.content.trim().is_empty() {
                Some("empty content".to_string())
            } else if c.embedding.len() != EMBEDDING_DIM {
                Some(format!("embedding has {} dims, expected {EMBEDDING_DIM}", c.embedding.len()))
            } else if c.risk == "high" && c.disclaimer.is_none() {
                Some("high risk chunk without disclaimer".to_string())
            } else {
                None
            };
            if let Some(why) = why {
                rej.add("doc_chunks", &format!("{}#{}", doc.doc_key, c.chunk_index),
                        &why, &c.heading);
                continue;
            }

            let r = sqlx::query(
                "insert into doc_chunks (id, document_id, dealer_id, chunk_index, heading,
                     content, token_count, embedding, embedding_model, vehicle_id,
                     audience, risk, disclaimer, metadata, content_hash)
                 values ($1,$2,$3,$4,$5,$6,$7,$8,$9,null,$10,$11,$12,$13,$14)
                 on conflict (document_id, chunk_index) do update set
                     heading = excluded.heading, content = excluded.content,
                     token_count = excluded.token_count, embedding = excluded.embedding,
                     embedding_model = excluded.embedding_model,
                     audience = excluded.audience, risk = excluded.risk,
                     disclaimer = excluded.disclaimer, metadata = excluded.metadata,
                     content_hash = excluded.content_hash",
            )
            .bind(uid(&["chunk", &doc_id.to_string(), &c.chunk_index.to_string()]))
            .bind(doc_id)
            .bind(dealer_id)
            .bind(c.chunk_index)
            .bind(&c.heading)
            .bind(&c.content)
            .bind(c.token_count)
            .bind(pgvector::Vector::from(c.embedding.clone()))
            .bind(&c.embedding_model)
            .bind(&c.audience)
            .bind(&c.risk)
            .bind(&c.disclaimer)
            .bind(&c.metadata)
            .bind(&c.content_hash)
            .execute(&mut **tx)
            .await?;
            n_chunk += r.rows_affected();
        }
    }
    Ok((n_doc, n_chunk))
}

// ------------------------------------------------------------ dealer config

/// Settings, salespeople, opening hours and holiday exceptions. Hand-written
/// config rather than generated data, so it lives in its own file.
async fn load_dealer_config(tx: &mut Transaction<'_, Postgres>, dealer_id: Uuid) -> Result<(u64, u64)> {
    let cfg: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(path("seed/dealer_config.json"))
            .context("seed/dealer_config.json missing")?,
    )?;
    let s = &cfg["settings"];
    let text = |k: &str| s[k].as_str().map(String::from);
    let int = |k: &str, d: i64| s[k].as_i64().unwrap_or(d) as i32;

    sqlx::query(
        "insert into dealer_settings (dealer_id, lead_email, crm_adf_email, notify_emails,
             timezone, summary_language, appointment_minutes, buffer_minutes,
             lead_idle_minutes, ai_disclosure_en, ai_disclosure_es)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
         on conflict (dealer_id) do update set
             lead_email = excluded.lead_email, crm_adf_email = excluded.crm_adf_email,
             notify_emails = excluded.notify_emails, timezone = excluded.timezone,
             summary_language = excluded.summary_language,
             appointment_minutes = excluded.appointment_minutes,
             buffer_minutes = excluded.buffer_minutes,
             lead_idle_minutes = excluded.lead_idle_minutes,
             ai_disclosure_en = excluded.ai_disclosure_en,
             ai_disclosure_es = excluded.ai_disclosure_es",
    )
    .bind(dealer_id)
    .bind(text("lead_email"))
    .bind(text("crm_adf_email"))
    .bind(
        s["notify_emails"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>())
            .unwrap_or_default(),
    )
    .bind(text("timezone").unwrap_or_else(|| "America/Chicago".into()))
    .bind(text("summary_language").unwrap_or_else(|| "en".into()))
    .bind(int("appointment_minutes", 30))
    .bind(int("buffer_minutes", 15))
    .bind(int("lead_idle_minutes", 15))
    .bind(text("ai_disclosure_en"))
    .bind(text("ai_disclosure_es"))
    .execute(&mut **tx)
    .await?;

    let mut n_res = 0;
    for r in cfg["resources"].as_array().into_iter().flatten() {
        let name = r["name"].as_str().unwrap_or_default();
        sqlx::query(
            "insert into resources (id, dealer_id, name, kind) values ($1,$2,$3,$4)
             on conflict (id) do update set name = excluded.name, kind = excluded.kind",
        )
        .bind(uid(&["resource", &dealer_id.to_string(), name]))
        .bind(dealer_id)
        .bind(name)
        .bind(r["kind"].as_str().unwrap_or("salesperson"))
        .execute(&mut **tx)
        .await?;
        n_res += 1;
    }

    // hours are replaced wholesale: a config file is the source of truth, and a
    // stale split-shift row left behind would silently keep offering times
    sqlx::query("delete from business_hours where dealer_id = $1")
        .bind(dealer_id)
        .execute(&mut **tx)
        .await?;
    let mut n_hours = 0;
    for h in cfg["business_hours"].as_array().into_iter().flatten() {
        sqlx::query(
            "insert into business_hours (id, dealer_id, weekday, opens, closes)
             values ($1, $2, $3, $4::time, $5::time)",
        )
        .bind(uid(&["hours", &dealer_id.to_string(), &h["weekday"].to_string(),
                    h["opens"].as_str().unwrap_or_default()]))
        .bind(dealer_id)
        .bind(h["weekday"].as_i64().unwrap_or(0) as i16)
        .bind(h["opens"].as_str())
        .bind(h["closes"].as_str())
        .execute(&mut **tx)
        .await?;
        n_hours += 1;
    }

    sqlx::query("delete from availability_exceptions where dealer_id = $1")
        .bind(dealer_id)
        .execute(&mut **tx)
        .await?;
    for e in cfg["availability_exceptions"].as_array().into_iter().flatten() {
        sqlx::query(
            "insert into availability_exceptions (id, dealer_id, date, opens, closes, reason)
             values ($1, $2, $3::date, $4::time, $5::time, $6)",
        )
        .bind(uid(&["exception", &dealer_id.to_string(), e["date"].as_str().unwrap_or_default()]))
        .bind(dealer_id)
        .bind(e["date"].as_str())
        .bind(e["opens"].as_str())
        .bind(e["closes"].as_str())
        .bind(e["reason"].as_str())
        .execute(&mut **tx)
        .await?;
    }
    Ok((n_res, n_hours))
}

// ---------------------------------------------------------------------- main

async fn run(pool: &PgPool, rej: &mut Rejects) -> Result<()> {
    let inv: Inventory = serde_json::from_str(
        &fs::read_to_string(path("seed/inventory.json"))
            .context("seed/inventory.json missing - run scripts/gen_inventory.py")?,
    )?;

    // dataset 1: inventory
    let mut tx = pool.begin().await?;
    let dealer_id = load_dealer(&mut tx, &inv.dealer).await?;
    let (map, n_veh) = load_vehicles(&mut tx, dealer_id, &inv, rej).await?;
    let n_int = load_internal(&mut tx, dealer_id, &inv, &map).await?;
    let n_photo = load_photos(&mut tx, dealer_id, &inv, &map, rej).await?;
    tx.commit().await?;
    println!("inventario: {n_veh} vehiculos, {n_int} internos, {n_photo} fotos");

    // dealer configuration: settings, salespeople, hours
    let mut tx = pool.begin().await?;
    let (n_res, n_hours) = load_dealer_config(&mut tx, dealer_id).await?;
    tx.commit().await?;
    println!("dealer config: {n_res} vendedores, {n_hours} franjas de horario");

    // dataset 2: model specs
    let mut tx = pool.begin().await?;
    let n_spec = load_trim_specs(&mut tx, rej).await?;
    tx.commit().await?;
    println!("trim_specs: {n_spec} modelos");

    // dataset 3: RAG corpus
    let mut tx = pool.begin().await?;
    let (n_doc, n_chunk) = load_corpus(&mut tx, dealer_id, rej).await?;
    tx.commit().await?;
    println!("corpus: {n_doc} documentos, {n_chunk} chunks");

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url()?)
        .await
        .context("no se pudo conectar a Postgres")?;

    // The same files sqlx-cli runs, tracked in the same _sqlx_migrations table,
    // so the two never disagree. Embedding them means a container can bring an
    // empty database up to date with no extra tooling installed.
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    let mut rej = Rejects::new();
    let result = run(&pool, &mut rej).await;
    // write the rejects even if a dataset blew up: losing them is exactly the
    // silent drop this loader exists to prevent
    rej.write()?;
    result
}
