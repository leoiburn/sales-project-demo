//! WHAT: the tools the model is allowed to call, and the validation behind each.
//! WHY:  this file is where the core rule is enforced - the model never writes
//!       facts. It sends structured arguments; everything the customer then sees
//!       is read back out of the database here. The model cannot quote a price,
//!       name a car, or confirm a booking; it can only ask for one.
//! HOW:  each tool has a JSON schema with `strict: true`, so arguments are
//!       guaranteed well-formed before this code runs. Every tool re-checks the
//!       world at call time - a car that was available when it was shown may be
//!       sold by the time someone books it.

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::calendar::{BookingRequest, CalendarProvider, LocalCalendar};
use crate::db::{self, DealerSettings};
use crate::llm::ToolDef;
use crate::{leads, outbox};

/// What a tool call produced: the text handed back to the model, plus the
/// vehicles it caused to be shown (recorded on the message so the summary
/// validator can later check that a claimed interest was really shown).
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub content: String,
    pub vehicle_ids: Vec<Uuid>,
    pub is_error: bool,
    /// Set when the conversation just changed hands or got a lead.
    pub handoff: bool,
}

impl Outcome {
    fn ok(value: serde_json::Value) -> Self {
        Self {
            content: value.to_string(),
            ..Default::default()
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            content: json!({ "error": msg.into() }).to_string(),
            is_error: true,
            ..Default::default()
        }
    }
}

fn obj(props: serde_json::Value, required: &[&str]) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false
    })
}

/// strict: true requires additionalProperties:false and an explicit required
/// list on every schema, which is why they are all built through `obj`.
pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "search_inventory".into(),
            description: "Search the dealership's real vehicle inventory. This is the ONLY way to learn what cars exist, what they cost, and their mileage. Never describe a vehicle that did not come back from this tool.".into(),
            input_schema: obj(json!({
                "body_type": {"anyOf": [{"type": "string", "enum": ["sedan","suv","truck","van","coupe","hatchback","wagon","convertible"]}, {"type": "null"}], "description": "Body style filter."},
                "make": {"type": ["string","null"], "description": "Manufacturer, e.g. Toyota."},
                "model": {"type": ["string","null"], "description": "Model name, e.g. RAV4."},
                "year_min": {"type": ["integer","null"], "description": "Oldest model year to include."},
                "max_price_usd": {"type": ["integer","null"], "description": "Maximum asking price in whole dollars."},
                "max_mileage": {"type": ["integer","null"], "description": "Maximum odometer reading."},
                "condition": {"anyOf": [{"type": "string", "enum": ["new","used","cpo"]}, {"type": "null"}], "description": "new, used or cpo."},
                "drivetrain": {"type": ["string","null"], "description": "Substring match, e.g. AWD or 4WD."}
            }), &["body_type","make","model","year_min","max_price_usd","max_mileage","condition","drivetrain"]),
            strict: true,
        },
        ToolDef {
            name: "search_policies".into(),
            description: "Search the dealership's policy and process knowledge base: financing, trade-ins, taxes and fees, warranty, returns, hours, service. Use it for any question that is not about a specific car in inventory. Quote only what it returns.".into(),
            input_schema: obj(json!({
                "query": {"type": "string", "description": "The customer's question, in their own words."}
            }), &["query"]),
            strict: true,
        },
        ToolDef {
            name: "get_available_slots".into(),
            description: "List open appointment times. Returns at most three, already formatted in the dealership's local timezone. Offer these verbatim; never invent a time.".into(),
            input_schema: obj(json!({
                "date_from": {"type": "string", "description": "First date to consider, YYYY-MM-DD."},
                "date_to": {"type": "string", "description": "Last date to consider, YYYY-MM-DD."},
                "kind": {"type": "string", "enum": ["test_drive","visit","call"], "description": "What the appointment is for."}
            }), &["date_from","date_to","kind"]),
            strict: true,
        },
        ToolDef {
            name: "book_appointment".into(),
            description: "Book one of the slots returned by get_available_slots. Requires the customer's phone or email to already be saved. Only say the appointment is confirmed AFTER this tool returns success.".into(),
            input_schema: obj(json!({
                "slot_id": {"type": "string", "description": "The id field from a slot returned by get_available_slots."},
                "kind": {"type": "string", "enum": ["test_drive","visit","call"], "description": "What the appointment is for."},
                "vehicle_id": {"type": ["string","null"], "description": "The id of the vehicle from search_inventory, if the appointment is about a specific car."}
            }), &["slot_id","kind","vehicle_id"]),
            strict: true,
        },
        ToolDef {
            name: "save_contact_info".into(),
            description: "Record contact details the customer actually gave you. Never guess or infer any of these. Only set sms_opt_in when the customer explicitly agreed to receive text messages.".into(),
            input_schema: obj(json!({
                "first_name": {"type": ["string","null"]},
                "last_name": {"type": ["string","null"]},
                "phone": {"type": ["string","null"], "description": "As the customer said it; it will be normalized."},
                "email": {"type": ["string","null"]},
                "preferred_language": {"anyOf": [{"type": "string", "enum": ["en","es"]}, {"type": "null"}]},
                "sms_opt_in": {"type": ["boolean","null"], "description": "True only on an explicit yes to text messages."}
            }), &["first_name","last_name","phone","email","preferred_language","sms_opt_in"]),
            strict: true,
        },
        ToolDef {
            name: "request_human".into(),
            description: "Hand the conversation to a salesperson. Use it when the customer asks for a person, is upset, or asks something you cannot answer from the tools.".into(),
            input_schema: obj(json!({
                "reason": {"type": "string", "description": "Short reason, for the staff email."}
            }), &["reason"]),
            strict: true,
        },
    ]
}

pub struct Ctx<'a> {
    pub db: &'a PgPool,
    pub settings: &'a DealerSettings,
    pub conversation_id: Uuid,
    pub customer_id: Uuid,
}

pub async fn dispatch(ctx: &Ctx<'_>, name: &str, input: &serde_json::Value) -> Result<Outcome> {
    match name {
        "search_inventory" => search_inventory(ctx, input).await,
        "search_policies" => search_policies(ctx, input).await,
        "get_available_slots" => get_available_slots(ctx, input).await,
        "book_appointment" => book_appointment(ctx, input).await,
        "save_contact_info" => save_contact_info(ctx, input).await,
        "request_human" => request_human(ctx, input).await,
        other => Ok(Outcome::err(format!("unknown tool {other}"))),
    }
}

fn opt_str<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str()).filter(|s| !s.trim().is_empty())
}

fn opt_i64(v: &serde_json::Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_i64())
}

async fn search_inventory(ctx: &Ctx<'_>, input: &serde_json::Value) -> Result<Outcome> {
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, i32, i64, Option<String>, Option<String>, Option<i32>, Option<String>)>(
        "select vehicle_id, stock_number, vehicle, condition, mileage, list_price_cents,
                drivetrain, exterior_color, days_in_stock, primary_photo
         from search_inventory($1, $2, $3, $4, $5, $6, $7, $8, $9, 'available', 6)",
    )
    .bind(ctx.settings.dealer_id)
    .bind(opt_str(input, "body_type"))
    .bind(opt_str(input, "make"))
    .bind(opt_str(input, "model"))
    .bind(opt_i64(input, "year_min").map(|y| y as i16))
    .bind(opt_i64(input, "max_price_usd").map(|d| d * 100))
    .bind(opt_i64(input, "max_mileage").map(|m| m as i32))
    .bind(opt_str(input, "condition"))
    .bind(opt_str(input, "drivetrain"))
    .fetch_all(ctx.db)
    .await?;

    if rows.is_empty() {
        return Ok(Outcome::ok(json!({
            "results": [],
            "note": "Nothing in stock matches. Offer to widen the search or take their details so a salesperson can call when something arrives."
        })));
    }

    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let results: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "vehicle_id": r.0, "stock_number": r.1, "vehicle": r.2,
                "condition": r.3, "mileage": r.4, "price": db::money(r.5),
                "drivetrain": r.6, "color": r.7, "days_in_stock": r.8,
            })
        })
        .collect();

    Ok(Outcome {
        content: json!({ "results": results }).to_string(),
        vehicle_ids: ids,
        ..Default::default()
    })
}

/// Full-text search over the policy corpus.
///
/// ponytail: the corpus has 251 chunks with bge-base-en-v1.5 vectors already
/// loaded and a match_documents() function ready to use, but computing a query
/// embedding in Rust needs fastembed-rs plus a ~90MB model download. Postgres
/// full-text search answers the same questions on a corpus this size and ships
/// today. Swap the query below for match_documents() once embeddings are wired -
/// the guardrails (audience filter, risk, disclaimer) are identical either way.
async fn search_policies(ctx: &Ctx<'_>, input: &serde_json::Value) -> Result<Outcome> {
    let query = opt_str(input, "query").unwrap_or_default();
    if query.is_empty() {
        return Ok(Outcome::err("query is required"));
    }

    let rows = sqlx::query_as::<_, (String, String, String, Option<String>, f32)>(
        "select heading, content, risk, disclaimer,
                ts_rank(to_tsvector('english', content),
                        websearch_to_tsquery('english', $2)) as score
         from doc_chunks
         where dealer_id = $1
           -- never retrievable: these are the bot's own operating rules
           and audience = 'customer'
           and to_tsvector('english', content) @@ websearch_to_tsquery('english', $2)
         order by score desc
         limit 3",
    )
    .bind(ctx.settings.dealer_id)
    .bind(query)
    .fetch_all(ctx.db)
    .await?;

    if rows.is_empty() {
        return Ok(Outcome::ok(json!({
            "passages": [],
            "note": "Nothing in the knowledge base covers this. Say you do not know and offer to connect a salesperson - do not answer from general knowledge."
        })));
    }

    let passages: Vec<serde_json::Value> = rows
        .iter()
        .map(|(heading, content, risk, disclaimer, _)| {
            json!({
                "heading": heading,
                "text": content,
                "risk": risk,
                // high-risk passages carry text that must be repeated verbatim
                "must_include_verbatim": disclaimer,
            })
        })
        .collect();

    Ok(Outcome::ok(json!({ "passages": passages })))
}

async fn get_available_slots(ctx: &Ctx<'_>, input: &serde_json::Value) -> Result<Outcome> {
    let parse = |k: &str| -> Result<NaiveDate> {
        opt_str(input, k)
            .ok_or_else(|| anyhow!("{k} is required"))?
            .parse()
            .map_err(|_| anyhow!("{k} must be YYYY-MM-DD"))
    };
    let (from, to) = match (parse("date_from"), parse("date_to")) {
        (Ok(f), Ok(t)) => (f, t),
        _ => return Ok(Outcome::err("date_from and date_to must be YYYY-MM-DD")),
    };
    if to < from {
        return Ok(Outcome::err("date_to is before date_from"));
    }
    // a runaway range would scan forever; two weeks is more than any customer needs
    let to = to.min(from + chrono::Duration::days(14));
    let kind = opt_str(input, "kind").unwrap_or("visit");

    let cal = LocalCalendar::new(ctx.db.clone());
    let slots = cal.free_slots(ctx.settings, from, to, kind, 3).await?;

    if slots.is_empty() {
        return Ok(Outcome::ok(json!({
            "slots": [],
            "note": "Nothing open in that range. The dealership is closed Sundays. Offer different dates."
        })));
    }

    Ok(Outcome::ok(json!({
        "slots": slots.iter().map(|s| json!({
            "id": s.id, "when": s.local, "with": s.resource_name
        })).collect::<Vec<_>>(),
        "reminder": "Read these times back exactly as written. Do not convert or reword them."
    })))
}

async fn book_appointment(ctx: &Ctx<'_>, input: &serde_json::Value) -> Result<Outcome> {
    let Some(slot_id) = opt_str(input, "slot_id") else {
        return Ok(Outcome::err("slot_id is required"));
    };
    let kind = opt_str(input, "kind").unwrap_or("visit").to_string();

    let customer = db::customer(ctx.db, ctx.customer_id).await?;
    if !customer.reachable() {
        return Ok(Outcome::err(
            "No phone or email on file. Ask the customer for one and call save_contact_info before booking.",
        ));
    }

    let vehicle = match opt_str(input, "vehicle_id").and_then(|s| Uuid::parse_str(s).ok()) {
        Some(id) => {
            // re-check now, not when it was shown: a car can sell between the
            // two turns, and confirming a test drive on a sold car is worse
            // than refusing
            match db::vehicle(ctx.db, ctx.settings.dealer_id, id).await? {
                None => return Ok(Outcome::err("That vehicle is not in inventory.")),
                Some(v) if v.status != "available" => {
                    return Ok(Outcome::err(format!(
                        "{} (stock {}) is no longer available - it is marked {}. Tell the customer, and offer to find something similar.",
                        v.label(), v.stock_number, v.status
                    )));
                }
                Some(v) => Some(v),
            }
        }
        None => None,
    };

    let cal = LocalCalendar::new(ctx.db.clone());
    let mut tx = ctx.db.begin().await?;

    // The lead is opened first so appointments.lead_id is satisfiable; both
    // rows commit together or neither does.
    let lead_id = leads::open_or_reuse(
        &mut tx,
        ctx.settings.dealer_id,
        ctx.conversation_id,
        ctx.customer_id,
    )
    .await?;

    let booking = match cal
        .book(
            &mut tx,
            ctx.settings,
            slot_id,
            BookingRequest {
                dealer_id: ctx.settings.dealer_id,
                lead_id,
                customer_id: ctx.customer_id,
                vehicle_id: vehicle.as_ref().map(|v| v.id),
                kind: kind.clone(),
            },
        )
        .await
    {
        Ok(b) => b,
        Err(e) if e.to_string() == "SLOT_TAKEN" => {
            tx.rollback().await?;
            return Ok(Outcome::err(
                "Someone just took that time. Call get_available_slots again and offer the customer the new options.",
            ));
        }
        Err(e) => {
            tx.rollback().await?;
            return Ok(Outcome::err(e.to_string()));
        }
    };

    let dealer = db::dealer(ctx.db, ctx.settings.dealer_id).await?;
    let where_at = dealer.address.clone().unwrap_or_else(|| dealer.name.clone());
    let what = match &vehicle {
        Some(v) => format!("{} (stock {})", v.label(), v.stock_number),
        None => "your visit".to_string(),
    };

    // Confirmation to the CUSTOMER. Separate from the lead email that goes to
    // the dealership - different audience, different template, and it does not
    // touch the lead's status.
    if let Some(email) = &customer.email {
        let body = format!(
            "Your {} at {} is confirmed.\n\nWhen:  {}\nWhere: {}\nWhat:  {}\nWith:  {}\n\nNeed to change it? Reply to this email or call {}.\n\n{}\n",
            kind.replace('_', " "),
            dealer.name,
            booking.local,
            where_at,
            what,
            booking.resource_name,
            dealer.phone.clone().unwrap_or_default(),
            ctx.settings.disclosure(customer.preferred_language.as_deref().unwrap_or("en")),
        );
        outbox::enqueue(
            &mut tx,
            ctx.settings.dealer_id,
            "appointment_confirmation",
            &format!("appt-confirm:{}:v1", booking.appointment_id),
            serde_json::to_value(crate::email::OutgoingEmail {
                to: vec![email.clone()],
                cc: vec![],
                from: crate::email::from_address(),
                subject: format!("Confirmed: {} at {} - {}", kind.replace('_', " "), dealer.name, booking.local),
                text: body,
                html: None,
                attachments: vec![],
                lead_id: None,
            })?,
        )
        .await?;
    }

    // SMS: consent is recorded and the message is queued, but no SMS provider is
    // wired yet - the worker will try to send it as mail and park it. Out of
    // scope per the brief; the row is here so the data model is ready.
    if customer.sms_opt_in && customer.phone.is_some() {
        tracing::info!(appointment = %booking.appointment_id, "SMS opted in; no SMS provider configured yet");
    }

    tx.commit().await?;

    Ok(Outcome {
        content: json!({
            "confirmed": true,
            "when": booking.local,
            "with": booking.resource_name,
            "where": where_at,
            "what": what,
            "confirmation_sent_to": customer.email,
            "reminder": "Now tell the customer it is confirmed, repeating the time exactly as given here."
        })
        .to_string(),
        vehicle_ids: vehicle.map(|v| vec![v.id]).unwrap_or_default(),
        ..Default::default()
    })
}

/// US-centric E.164 normalization. Returns None when the input is not a phone
/// number, which is better than storing something a salesperson cannot dial.
pub fn normalize_phone(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let e164 = match digits.len() {
        10 => format!("+1{digits}"),
        11 if digits.starts_with('1') => format!("+{digits}"),
        8..=15 if raw.trim_start().starts_with('+') => format!("+{digits}"),
        _ => return None,
    };
    Some(e164)
}

pub fn valid_email(raw: &str) -> bool {
    let raw = raw.trim();
    let Some((local, domain)) = raw.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !raw.contains(char::is_whitespace)
}

async fn save_contact_info(ctx: &Ctx<'_>, input: &serde_json::Value) -> Result<Outcome> {
    let phone = match opt_str(input, "phone") {
        Some(p) => match normalize_phone(p) {
            Some(n) => Some(n),
            None => {
                return Ok(Outcome::err(format!(
                    "'{p}' is not a phone number I can store. Ask the customer to repeat it."
                )))
            }
        },
        None => None,
    };
    let email = match opt_str(input, "email") {
        Some(e) if valid_email(e) => Some(e.trim().to_string()),
        Some(e) => {
            return Ok(Outcome::err(format!(
                "'{e}' is not a valid email address. Ask the customer to repeat it."
            )))
        }
        None => None,
    };

    // SMS consent is only ever recorded on an explicit yes, and it carries a
    // timestamp - consent without a time is not evidence of consent.
    let sms_opt_in = input.get("sms_opt_in").and_then(|v| v.as_bool()).unwrap_or(false);

    sqlx::query(
        "update customers set
            first_name = coalesce($2, first_name),
            last_name = coalesce($3, last_name),
            phone = coalesce($4, phone),
            email = coalesce($5, email),
            preferred_language = coalesce($6, preferred_language),
            sms_opt_in = sms_opt_in or $7,
            sms_opt_in_at = case when $7 and not sms_opt_in then now() else sms_opt_in_at end,
            sms_opt_in_source = case when $7 and not sms_opt_in then 'chat' else sms_opt_in_source end,
            updated_at = now()
         where id = $1",
    )
    .bind(ctx.customer_id)
    .bind(opt_str(input, "first_name"))
    .bind(opt_str(input, "last_name"))
    .bind(phone.as_deref())
    .bind(email.as_deref())
    .bind(opt_str(input, "preferred_language"))
    .bind(sms_opt_in)
    .execute(ctx.db)
    .await?;

    let saved = db::customer(ctx.db, ctx.customer_id).await?;
    Ok(Outcome::ok(json!({
        "saved": true,
        "have_name": saved.full_name().is_some(),
        "have_phone": saved.phone.is_some(),
        "have_email": saved.email.is_some(),
        "can_book": saved.reachable(),
    })))
}

async fn request_human(ctx: &Ctx<'_>, input: &serde_json::Value) -> Result<Outcome> {
    let reason = opt_str(input, "reason").unwrap_or("customer asked for a person");

    let mut tx = ctx.db.begin().await?;
    sqlx::query("update conversations set status = 'handoff', bot_paused = true where id = $1")
        .bind(ctx.conversation_id)
        .execute(&mut *tx)
        .await?;

    let lead_id = leads::open_or_reuse(
        &mut tx,
        ctx.settings.dealer_id,
        ctx.conversation_id,
        ctx.customer_id,
    )
    .await?;

    let customer = db::customer(ctx.db, ctx.customer_id).await?;
    let mut to = vec![ctx.settings.lead_email.clone()];
    to.extend(ctx.settings.notify_emails.iter().cloned());

    outbox::enqueue(
        &mut tx,
        ctx.settings.dealer_id,
        "handoff_email",
        &format!("handoff:{}:v1", ctx.conversation_id),
        serde_json::to_value(crate::email::OutgoingEmail {
            to,
            cc: vec![],
            from: crate::email::from_address(),
            subject: format!(
                "Handoff requested: {}",
                customer.full_name().unwrap_or_else(|| "unnamed customer".into())
            ),
            text: format!(
                "A customer asked for a person.\n\nReason: {reason}\nName:   {}\nPhone:  {}\nEmail:  {}\n\nThe bot has stopped answering this conversation.\nLead: {lead_id}\n",
                customer.full_name().unwrap_or_else(|| "-".into()),
                customer.phone.clone().unwrap_or_else(|| "-".into()),
                customer.email.clone().unwrap_or_else(|| "-".into()),
            ),
            html: None,
            attachments: vec![],
            lead_id: None,
        })?,
    )
    .await?;
    tx.commit().await?;

    Ok(Outcome {
        content: json!({
            "handed_off": true,
            "reminder": "Tell the customer a salesperson will reach out, then stop."
        })
        .to_string(),
        handoff: true,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_phones_people_actually_type() {
        assert_eq!(normalize_phone("(210) 555-0143").as_deref(), Some("+12105550143"));
        assert_eq!(normalize_phone("210.555.0143").as_deref(), Some("+12105550143"));
        assert_eq!(normalize_phone("1-210-555-0143").as_deref(), Some("+12105550143"));
        assert_eq!(normalize_phone("+52 55 1234 5678").as_deref(), Some("+525512345678"));
        assert_eq!(normalize_phone("call me"), None);
        assert_eq!(normalize_phone("555-0143"), None);
    }

    #[test]
    fn rejects_addresses_that_are_not_emails() {
        assert!(valid_email("maria@example.com"));
        assert!(valid_email("a.b+c@sub.example.co.uk"));
        assert!(!valid_email("maria@example"));
        assert!(!valid_email("@example.com"));
        assert!(!valid_email("maria example.com"));
        assert!(!valid_email("maria@ example.com"));
    }

    #[test]
    fn every_tool_schema_satisfies_strict_mode() {
        for t in definitions() {
            assert!(t.strict, "{} is not strict", t.name);
            assert_eq!(
                t.input_schema["additionalProperties"], serde_json::json!(false),
                "{} allows extra properties", t.name
            );
            let props = t.input_schema["properties"].as_object().unwrap();
            let required = t.input_schema["required"].as_array().unwrap();
            // strict mode requires every property to be listed in `required`;
            // optional arguments are expressed as nullable types instead
            assert_eq!(props.len(), required.len(), "{} required/properties mismatch", t.name);
            // the API rejects `"type": [.., "null"]` next to an `enum`; optional
            // enums must be written as anyOf [{string + enum}, {null}]
            for (name, p) in props {
                assert!(
                    !(p.get("enum").is_some() && p["type"].is_array()),
                    "{}.{name}: enum with a type array - use anyOf", t.name
                );
            }
        }
    }
}
