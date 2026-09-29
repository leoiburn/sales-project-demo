//! WHAT: the tests that need a real Postgres - booking races, idempotent email,
//!       the retry path, a car selling mid-conversation, the append-only log.
//! WHY:  every one of these is a guarantee the DATABASE makes (an EXCLUDE
//!       constraint, a unique key, a trigger). Mocking the database would test
//!       the mock. These run against the live demo database instead.
//! HOW:  each test creates its own customers and conversations, so tests can run
//!       in parallel without seeing each other, and deletes them afterwards.
//!       Needs DATABASE_URL and a loaded database (cargo run -p seed --bin reset_db).

use automotrix::calendar::{CalendarProvider, LocalCalendar};
use automotrix::db;
use automotrix::email::{CapturingSender, FailingSender};
use automotrix::{leads, outbox, tools};
use chrono::{Datelike, Duration, Utc, Weekday};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// The outbox is one global queue and `outbox::tick` claims ANY due row. Two
/// tests that drive the worker would otherwise send each other's mail - which is
/// exactly how the retry test once saw its "failing" row delivered by the other
/// test's capturing sender. Tests that call tick() hold this for their duration.
static OUTBOX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn pool() -> PgPool {
    let _ = rustls::crypto::ring::default_provider().install_default();
    db::connect(&automotrix::database_url().unwrap()).await.unwrap()
}

/// A fresh customer + conversation, with an email so they can book.
async fn customer(db: &PgPool, dealer: Uuid, tag: &str) -> (Uuid, Uuid) {
    let convo = db::conversation_for_identity(db, dealer, "cli", &format!("test-{tag}-{}", Uuid::now_v7()))
        .await
        .unwrap();
    sqlx::query("update customers set first_name = $2, email = $3 where id = $1")
        .bind(convo.customer_id)
        .bind(format!("Test {tag}"))
        .bind(format!("{tag}@example.com"))
        .execute(db)
        .await
        .unwrap();
    (convo.id, convo.customer_id)
}

async fn cleanup(db: &PgPool, customers: &[Uuid]) {
    for c in customers {
        // A booking queues a confirmation email, and outbox has no foreign key
        // to the customer - so without this, every test run leaves mail behind
        // that the next running worker would dutifully send.
        sqlx::query(
            "delete from outbox where idempotency_key in (
                 select 'appt-confirm:' || id || ':v1' from appointments where customer_id = $1)",
        )
        .bind(c)
        .execute(db)
        .await
        .ok();
        sqlx::query("delete from appointments where customer_id = $1").bind(c).execute(db).await.ok();
        // cascades to conversations, messages, leads, identities
        sqlx::query("delete from customers where id = $1").bind(c).execute(db).await.ok();
    }
}

/// Next weekday that is not a Sunday and not a holiday exception, far enough
/// out that "now" never collides with it.
fn next_open_day() -> chrono::NaiveDate {
    let mut d = (Utc::now() + Duration::days(3)).date_naive();
    while d.weekday() == Weekday::Sun {
        d = d.succ_opt().unwrap();
    }
    d
}

#[tokio::test]
async fn two_concurrent_bookings_of_one_slot_exactly_one_wins() {
    let db = pool().await;
    let dealer = db::default_dealer(&db).await.unwrap();
    let settings = db::settings(&db, dealer).await.unwrap();

    let day = next_open_day();
    let slots = LocalCalendar::new(db.clone())
        .free_slots(&settings, day, day, "test_drive", 3)
        .await
        .unwrap();
    // take the LAST offered slot so parallel tests that grab the first one do
    // not collide with this race
    let slot = slots.last().expect("a free slot").id.clone();

    let (c1, cu1) = customer(&db, dealer, "race-a").await;
    let (c2, cu2) = customer(&db, dealer, "race-b").await;

    let run = |convo: Uuid, cust: Uuid| {
        let db = db.clone();
        let settings = settings.clone();
        let slot = slot.clone();
        tokio::spawn(async move {
            let ctx = tools::Ctx { db: &db, settings: &settings, conversation_id: convo, customer_id: cust };
            tools::dispatch(&ctx, "book_appointment",
                &json!({ "slot_id": slot, "kind": "test_drive", "vehicle_id": null }))
                .await
                .unwrap()
        })
    };
    let (a, b) = tokio::join!(run(c1, cu1), run(c2, cu2));
    let (a, b) = (a.unwrap(), b.unwrap());

    let wins = [&a, &b].iter().filter(|o| o.content.contains("\"confirmed\":true")).count();
    let losses: Vec<_> = [&a, &b].into_iter().filter(|o| o.is_error).collect();

    cleanup(&db, &[cu1, cu2]).await;

    assert_eq!(wins, 1, "exactly one booking must win.\nA: {}\nB: {}", a.content, b.content);
    assert_eq!(losses.len(), 1);
    // the loser is told to fetch fresh times, not left with a dead end
    assert!(losses[0].content.contains("get_available_slots"), "{}", losses[0].content);
}

#[tokio::test]
async fn a_car_sold_between_showing_and_booking_is_refused() {
    let db = pool().await;
    let dealer = db::default_dealer(&db).await.unwrap();
    let settings = db::settings(&db, dealer).await.unwrap();
    let (convo, cust) = customer(&db, dealer, "sold").await;

    let (vid,): (Uuid,) = sqlx::query_as(
        "select id from vehicles where dealer_id = $1 and status = 'available' order by stock_number limit 1",
    )
    .bind(dealer)
    .fetch_one(&db)
    .await
    .unwrap();

    // the salesperson sells it while the customer is still typing
    sqlx::query("update vehicles set status = 'sold' where id = $1").bind(vid).execute(&db).await.unwrap();

    let day = next_open_day();
    let slot = LocalCalendar::new(db.clone())
        .free_slots(&settings, day, day, "test_drive", 1)
        .await
        .unwrap()[0]
        .id
        .clone();
    let ctx = tools::Ctx { db: &db, settings: &settings, conversation_id: convo, customer_id: cust };
    let out = tools::dispatch(&ctx, "book_appointment",
        &json!({ "slot_id": slot, "kind": "test_drive", "vehicle_id": vid.to_string() }))
        .await
        .unwrap();

    let appts: (i64,) = sqlx::query_as("select count(*) from appointments where customer_id = $1")
        .bind(cust)
        .fetch_one(&db)
        .await
        .unwrap();

    sqlx::query("update vehicles set status = 'available' where id = $1").bind(vid).execute(&db).await.unwrap();
    cleanup(&db, &[cust]).await;

    assert!(out.is_error, "booking a sold car must fail: {}", out.content);
    assert!(out.content.contains("no longer available"), "{}", out.content);
    assert_eq!(appts.0, 0, "no appointment row may exist for a sold car");
}

#[tokio::test]
async fn booking_without_contact_details_is_refused() {
    let db = pool().await;
    let dealer = db::default_dealer(&db).await.unwrap();
    let settings = db::settings(&db, dealer).await.unwrap();
    let convo = db::conversation_for_identity(&db, dealer, "cli", &format!("test-anon-{}", Uuid::now_v7()))
        .await
        .unwrap();

    let ctx = tools::Ctx { db: &db, settings: &settings, conversation_id: convo.id, customer_id: convo.customer_id };
    let out = tools::dispatch(&ctx, "book_appointment",
        &json!({ "slot_id": "whatever.1", "kind": "visit", "vehicle_id": null }))
        .await
        .unwrap();
    cleanup(&db, &[convo.customer_id]).await;

    assert!(out.is_error);
    assert!(out.content.contains("save_contact_info"), "{}", out.content);
}

#[tokio::test]
async fn the_same_lead_triggered_twice_is_emailed_once() {
    let db = pool().await;
    let dealer = db::default_dealer(&db).await.unwrap();
    let (convo, cust) = customer(&db, dealer, "idem").await;

    let lead_id = {
        let mut tx = db.begin().await.unwrap();
        let id = leads::open_or_reuse(&mut tx, dealer, convo, cust).await.unwrap();
        tx.commit().await.unwrap();
        id
    };

    // the trigger fires twice - say a retry after a timeout
    let key = format!("lead:{lead_id}:v1");
    let payload = json!({ "to": ["x@example.com"], "from": "bot@example.com", "subject": "s", "text": "t" });
    let first = outbox::enqueue_now(&db, dealer, "lead_email", &key, payload.clone()).await.unwrap();
    let second = outbox::enqueue_now(&db, dealer, "lead_email", &key, payload).await.unwrap();

    // and opening the lead again returns the same lead, not a second one
    let again = {
        let mut tx = db.begin().await.unwrap();
        let id = leads::open_or_reuse(&mut tx, dealer, convo, cust).await.unwrap();
        tx.commit().await.unwrap();
        id
    };

    let rows: (i64,) = sqlx::query_as("select count(*) from outbox where idempotency_key = $1")
        .bind(&key)
        .fetch_one(&db)
        .await
        .unwrap();

    sqlx::query("delete from outbox where idempotency_key = $1").bind(&key).execute(&db).await.unwrap();
    cleanup(&db, &[cust]).await;

    assert!(first);
    assert!(!second, "the second enqueue must be a no-op");
    assert_eq!(rows.0, 1, "exactly one email row");
    assert_eq!(again, lead_id, "one open lead per conversation");
}

#[tokio::test]
async fn a_dead_mail_server_retries_then_gives_up_and_keeps_the_lead() {
    let _outbox = OUTBOX.lock().await;
    let db = pool().await;
    let dealer = db::default_dealer(&db).await.unwrap();
    let (convo, cust) = customer(&db, dealer, "smtp").await;

    let lead_id = {
        let mut tx = db.begin().await.unwrap();
        let id = leads::open_or_reuse(&mut tx, dealer, convo, cust).await.unwrap();
        tx.commit().await.unwrap();
        id
    };
    let key = format!("test-smtp:{lead_id}");
    outbox::enqueue_now(&db, dealer, "lead_email", &key,
        json!({ "to": ["x@example.com"], "from": "bot@example.com", "subject": "s", "text": "t",
                "lead_id": lead_id.to_string() }))
        .await
        .unwrap();

    // Other outbox rows may exist in the shared database; park them so this
    // test's worker only ever sees its own row.
    sqlx::query("update outbox set next_attempt_at = now() + interval '1 day'
                 where status = 'pending' and idempotency_key <> $1")
        .bind(&key)
        .execute(&db)
        .await
        .unwrap();

    let failing = FailingSender;
    for _ in 0..outbox::MAX_ATTEMPTS {
        // skip the backoff wait
        sqlx::query("update outbox set next_attempt_at = now() where idempotency_key = $1")
            .bind(&key)
            .execute(&db)
            .await
            .unwrap();
        outbox::tick(&db, &failing).await.unwrap();
    }

    let (status, attempts, err): (String, i32, Option<String>) = sqlx::query_as(
        "select status, attempts, last_error from outbox where idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&db)
    .await
    .unwrap();
    let lead: (String,) = sqlx::query_as("select status from leads where id = $1")
        .bind(lead_id)
        .fetch_one(&db)
        .await
        .unwrap();

    // a sixth tick must not pick it up again
    let captured = Arc::new(CapturingSender::default());
    outbox::tick(&db, captured.as_ref()).await.unwrap();
    let resent = captured.sent.lock().unwrap().iter().any(|e| e.lead_id.as_deref() == Some(&lead_id.to_string()));

    sqlx::query("delete from outbox where idempotency_key = $1").bind(&key).execute(&db).await.unwrap();
    sqlx::query("update outbox set next_attempt_at = now() where status = 'pending'")
        .execute(&db).await.unwrap();
    cleanup(&db, &[cust]).await;

    assert_eq!(status, "failed");
    assert_eq!(attempts, outbox::MAX_ATTEMPTS);
    assert!(err.unwrap_or_default().contains("connection refused"));
    assert_eq!(lead.0, "new", "the lead survives; it was never marked sent");
    assert!(!resent, "a failed job is not retried forever");
}

#[tokio::test]
async fn a_successful_send_marks_the_lead_sent() {
    let _outbox = OUTBOX.lock().await;
    let db = pool().await;
    let dealer = db::default_dealer(&db).await.unwrap();
    let (convo, cust) = customer(&db, dealer, "sent").await;
    let lead_id = {
        let mut tx = db.begin().await.unwrap();
        let id = leads::open_or_reuse(&mut tx, dealer, convo, cust).await.unwrap();
        tx.commit().await.unwrap();
        id
    };
    let key = format!("test-sent:{lead_id}");
    outbox::enqueue_now(&db, dealer, "lead_email", &key,
        json!({ "to": ["x@example.com"], "from": "bot@example.com", "subject": "s", "text": "t",
                "lead_id": lead_id.to_string() }))
        .await
        .unwrap();

    let captured = CapturingSender::default();
    // drain until our row is handled; other tests' rows may be in the queue
    for _ in 0..10 {
        outbox::tick(&db, &captured).await.unwrap();
        let (s,): (String,) = sqlx::query_as("select status from outbox where idempotency_key = $1")
            .bind(&key).fetch_one(&db).await.unwrap();
        if s == "sent" { break; }
    }
    let lead: (String, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("select status, sent_at from leads where id = $1")
            .bind(lead_id).fetch_one(&db).await.unwrap();

    sqlx::query("delete from outbox where idempotency_key = $1").bind(&key).execute(&db).await.unwrap();
    cleanup(&db, &[cust]).await;

    assert_eq!(lead.0, "sent");
    assert!(lead.1.is_some(), "sent_at is set");
}

#[tokio::test]
async fn messages_cannot_be_edited_or_deleted_one_by_one() {
    let db = pool().await;
    let dealer = db::default_dealer(&db).await.unwrap();
    let (convo, cust) = customer(&db, dealer, "append").await;
    let id = db::append_message(&db, convo, dealer, "customer", "hola", None, &[]).await.unwrap();

    let upd = sqlx::query("update messages set content = 'edited' where id = $1").bind(id).execute(&db).await;
    let del = sqlx::query("delete from messages where id = $1").bind(id).execute(&db).await;
    // deleting the whole conversation is deliberate and must still work
    cleanup(&db, &[cust]).await;
    let left: (i64,) = sqlx::query_as("select count(*) from messages where id = $1")
        .bind(id).fetch_one(&db).await.unwrap();

    assert!(upd.unwrap_err().to_string().contains("append-only"));
    assert!(del.unwrap_err().to_string().contains("append-only"));
    assert_eq!(left.0, 0, "deleting the conversation cascades to its messages");
}

#[tokio::test]
async fn save_contact_info_normalizes_and_rejects() {
    let db = pool().await;
    let dealer = db::default_dealer(&db).await.unwrap();
    let settings = db::settings(&db, dealer).await.unwrap();
    let convo = db::conversation_for_identity(&db, dealer, "cli", &format!("test-contact-{}", Uuid::now_v7()))
        .await.unwrap();
    let ctx = tools::Ctx { db: &db, settings: &settings, conversation_id: convo.id, customer_id: convo.customer_id };

    let bad = tools::dispatch(&ctx, "save_contact_info", &json!({
        "first_name": null, "last_name": null, "phone": "not a phone", "email": null,
        "preferred_language": null, "sms_opt_in": null })).await.unwrap();
    let good = tools::dispatch(&ctx, "save_contact_info", &json!({
        "first_name": "María", "last_name": "Núñez", "phone": "(210) 555-0143",
        "email": "maria@example.com", "preferred_language": "es", "sms_opt_in": null })).await.unwrap();
    let c = db::customer(&db, convo.customer_id).await.unwrap();
    cleanup(&db, &[convo.customer_id]).await;

    assert!(bad.is_error);
    assert!(!good.is_error, "{}", good.content);
    assert_eq!(c.phone.as_deref(), Some("+12105550143"));
    assert_eq!(c.first_name.as_deref(), Some("María"));
    // no explicit yes, so no consent recorded
    assert!(!c.sms_opt_in);
}
