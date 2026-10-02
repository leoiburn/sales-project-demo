//! WHAT: runs one conversational turn, and the background jobs that turn
//!       conversations into leads.
//! WHY:  this is where the pieces meet - customer message in, tool calls against
//!       the database, a reply out, and eventually an email to the dealership.
//! HOW:  a turn is a bounded tool loop: call the model, run any tools it asked
//!       for, feed ALL their results back in one message, repeat until it
//!       answers in text. The final text goes through the price guardrail before
//!       anyone sees it. On the first reply of a conversation the AI disclosure
//!       is prepended by code, so it cannot be forgotten or paraphrased away.
//!
//!       Leads are made by code, never by the model. Three triggers open one:
//!       a booking, a handoff, or contact details plus 15 quiet minutes. The
//!       worker then turns every 'new' lead into its emails.

use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

use crate::db::{self, Conversation, DealerSettings};
use crate::email::{EmailAttachment, OutgoingEmail};
use crate::llm::{Block, Message};
use crate::{adf, guardrail, leads, outbox, summary, tools, transcript, App};

const MAX_TOOL_ROUNDS: usize = 8;

/// Bare-bones language sniff for the first turn, before the customer has told us
/// anything. Wrong guesses are cheap: the model answers in whatever language the
/// customer writes in regardless, and save_contact_info records the real one.
pub fn detect_language(text: &str) -> &'static str {
    let t = text.to_lowercase();
    let markers = [
        "hola", "busco", "quiero", "precio", "cuánto", "cuanto", "gracias", "camioneta",
        "carro", "coche", "auto ", "tienen", "necesito", "buenas", "¿", "¡", "ñ",
    ];
    if markers.iter().any(|m| t.contains(m)) {
        "es"
    } else {
        "en"
    }
}

fn system_prompt(settings: &DealerSettings, dealer: &db::Dealer, lang: &str) -> String {
    let tz = settings.tz();
    let today = Utc::now().with_timezone(&tz).format("%A %Y-%m-%d").to_string();
    format!(
        r#"You are the AI sales assistant for {name}, an independent multi-make car dealership at {address}. Phone {phone}.

Today is {today} ({tz}). The dealership is closed Sundays.

HOW YOU WORK
- You never state a fact from memory. Every vehicle, price, mileage, rate, fee or policy you mention must come from a tool result in this conversation.
- For cars: call search_inventory. If a car is not in its results, it does not exist here - say so.
- For policies (financing, trade-ins, fees, warranty, returns, hours): call search_policies. If it returns a passage marked must_include_verbatim, repeat that text word for word in your answer.
- For appointments: call show_booking_form. The form collects name, phone, optional email, day and time, and books it. Don't ask for those details in chat. Only if the customer can't use the form (for example on a voice call), call get_available_slots, offer the times exactly as written, collect a name and phone or email with save_contact_info, then book_appointment. You may say "confirmed" only after a booking succeeds.
- Never guess a name, phone or email.
- Only record SMS consent if the customer explicitly says yes to text messages.
- If the customer asks for a person, is frustrated, or asks something the tools cannot answer, call request_human.
- Never promise approval, a specific rate, a trade-in value or a discount.

STYLE
- Answer in the customer's language. Their first message looks like {lang_name}.
- Sound like a real person texting, not a brochure: one to three short sentences, plain words, contractions, no bullet lists, no markdown (no **bold**, no "--" or "—" dashes, no headings), no "Great question!", no sign-offs.
- Answer only what was asked. Skip filler, repeated disclaimers and restating their question.
- When you show cars, name at most three in one line each: year make model, price, miles. The cards show the rest.
- Photos: every car you find with search_inventory appears as a card with its full photo gallery. When the customer asks for pictures, search for that car and tell them to use the arrows on its card photo to see the rest. Never say you can't send pictures.
- End with one easy next step or question, not several.""#,
        name = dealer.name,
        address = dealer.address.clone().unwrap_or_default(),
        phone = dealer.phone.clone().unwrap_or_default(),
        today = today,
        tz = settings.timezone,
        lang_name = if lang == "es" { "Spanish" } else { "English" },
    )
}

/// Rebuilds the model-facing history from the transcript. Only the words go
/// back in - tool calls and their raw results are an implementation detail of
/// each turn, and replaying them would bloat every request.
fn history(messages: &[db::Message]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    for m in messages {
        let role = match m.role.as_str() {
            "customer" => "user",
            "assistant" | "staff" => "assistant",
            _ => continue,
        };
        // the API rejects two consecutive turns from the same role that it
        // cannot merge cleanly across our schema, so fold them together
        if let Some(last) = out.last_mut() {
            if last.role == role {
                if let Some(Block::Text { text }) = last.content.first_mut() {
                    text.push_str("\n\n");
                    text.push_str(&m.content);
                    continue;
                }
            }
        }
        out.push(Message {
            role: role.into(),
            content: vec![Block::Text { text: m.content.clone() }],
        });
    }
    out
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Reply {
    pub text: String,
    pub vehicles: Vec<db::Vehicle>,
    pub handoff: bool,
    pub bot_paused: bool,
    pub form: Option<serde_json::Value>,
}

pub async fn turn(app: &App, convo: &Conversation, customer_text: &str) -> Result<Reply> {
    let settings = db::settings(&app.db, convo.dealer_id).await?;
    let dealer = db::dealer(&app.db, convo.dealer_id).await?;

    db::append_message(&app.db, convo.id, convo.dealer_id, "customer", customer_text, None, &[])
        .await?;
    db::touch_conversation(&app.db, convo.id).await?;

    // A human has taken over. The bot records the message and stays quiet.
    let live: (bool,) = sqlx::query_as("select bot_paused from conversations where id = $1")
        .bind(convo.id)
        .fetch_one(&app.db)
        .await?;
    if live.0 {
        return Ok(Reply {
            text: String::new(),
            vehicles: vec![],
            handoff: true,
            bot_paused: true,
            form: None,
        });
    }

    let all = db::messages(&app.db, convo.id).await?;
    let first_reply = !all.iter().any(|m| m.role == "assistant");
    let customer = db::customer(&app.db, convo.customer_id).await?;
    let lang = customer
        .preferred_language
        .clone()
        .unwrap_or_else(|| detect_language(customer_text).to_string());

    let system = system_prompt(&settings, &dealer, &lang);
    let tool_defs = tools::definitions();
    let ctx = tools::Ctx {
        db: &app.db,
        settings: &settings,
        conversation_id: convo.id,
        customer_id: convo.customer_id,
    };

    let mut msgs = history(&all);
    let mut shown: Vec<Uuid> = Vec::new();
    let mut calls_log: Vec<serde_json::Value> = Vec::new();
    let mut handoff = false;
    let mut form = None;
    let mut final_text = String::new();

    for _round in 0..MAX_TOOL_ROUNDS {
        let res = app.llm.complete(&system, &msgs, Some(&tool_defs), 1500).await?;

        if !res.wants_tools() {
            final_text = res.text();
            break;
        }

        let assistant = Message::assistant(res.content.clone());
        let mut results: Vec<Block> = Vec::new();
        for (id, name, input) in assistant.tool_uses() {
            let outcome = tools::dispatch(&ctx, name, input).await.unwrap_or_else(|e| {
                tracing::error!(tool = name, "tool failed: {e:#}");
                tools::Outcome {
                    content: json!({ "error": "internal error, apologize and offer a salesperson" })
                        .to_string(),
                    is_error: true,
                    ..Default::default()
                }
            });
            shown.extend(&outcome.vehicle_ids);
            handoff |= outcome.handoff;
            if outcome.form.is_some() {
                form = outcome.form.clone();
            }
            calls_log.push(json!({ "name": name, "input": input, "is_error": outcome.is_error }));
            results.push(Block::ToolResult {
                tool_use_id: id.to_string(),
                content: outcome.content,
                is_error: outcome.is_error,
            });
        }
        msgs.push(assistant);
        // all results for this turn in ONE user message
        msgs.push(Message::tool_results(results));
    }

    if final_text.is_empty() {
        final_text = if lang == "es" {
            "Déjame pasarte con alguien del equipo para ayudarte mejor.".into()
        } else {
            "Let me get someone from our team to help you with that.".into()
        };
    }

    // The last line of defence: nothing money-shaped the database cannot back.
    let allowed = guardrail::allowed_for_dealer(&app.db, convo.dealer_id).await?;
    let violations = guardrail::check(&final_text, &allowed);
    if !violations.is_empty() {
        db::record_guardrail_event(
            &app.db,
            Some(convo.id),
            convo.dealer_id,
            "reply_unbacked_figure",
            json!({ "violations": violations, "draft": final_text }),
        )
        .await?;

        // one correction attempt, with the specific figures named
        let bad: Vec<String> = violations.iter().map(|v| v.token.clone()).collect();
        msgs.push(Message::assistant(vec![Block::Text { text: final_text.clone() }]));
        msgs.push(Message::user_text(format!(
            "[system check] Your reply contains figures that are not in our inventory or knowledge base: {}. Rewrite the reply without them. Use only numbers that came back from a tool in this conversation.",
            bad.join(", ")
        )));
        let retry = app.llm.complete(&system, &msgs, None, 800).await?.text();
        final_text = if guardrail::check(&retry, &allowed).is_empty() && !retry.is_empty() {
            retry
        } else if lang == "es" {
            "Para darte cifras exactas prefiero que un asesor te las confirme. ¿Te comparto con alguien del equipo o te busco opciones en inventario?".into()
        } else {
            "I'd rather have one of our team confirm exact figures for you. Want me to connect you, or look through inventory together?".into()
        };
    }

    if first_reply {
        final_text = format!("{}\n\n{}", settings.disclosure(&lang), final_text);
    }

    shown.sort();
    shown.dedup();
    db::append_message(
        &app.db,
        convo.id,
        convo.dealer_id,
        "assistant",
        &final_text,
        if calls_log.is_empty() { None } else { Some(json!(calls_log)) },
        &shown,
    )
    .await?;
    db::touch_conversation(&app.db, convo.id).await?;

    let vehicles = db::vehicles_by_ids(&app.db, convo.dealer_id, &shown).await?;
    Ok(Reply {
        text: final_text,
        vehicles,
        handoff,
        bot_paused: handoff,
        form,
    })
}

// ---------------------------------------------------------------- leads

/// Conversations that went quiet with contact details on file become leads.
/// Nobody is present when a chat goes idle, so this runs from the worker.
///
/// It also sends the customer one follow-up email inviting them to book or ask
/// another question - the conversation stays open, so replying through the web
/// chat picks up exactly where they left off.
pub async fn idle_sweep(db: &PgPool) -> Result<usize> {
    let idle: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select c.id, c.dealer_id, c.customer_id
         from conversations c
         join customers cu on cu.id = c.customer_id
         join dealer_settings s on s.dealer_id = c.dealer_id
         where c.status = 'open'
           and c.last_message_at < now() - make_interval(mins => s.lead_idle_minutes)
           and (cu.phone is not null or cu.email is not null)
           and not exists (select 1 from leads l
                           where l.conversation_id = c.id and l.status in ('new','sent'))",
    )
    .fetch_all(db)
    .await?;

    for (convo_id, dealer_id, customer_id) in &idle {
        let mut tx = db.begin().await?;
        leads::open_or_reuse(&mut tx, *dealer_id, *convo_id, *customer_id).await?;

        let customer = db::customer(db, *customer_id).await?;
        if let Some(email) = &customer.email {
            let settings = db::settings(db, *dealer_id).await?;
            let dealer = db::dealer(db, *dealer_id).await?;
            let es = customer.preferred_language.as_deref() == Some("es");
            let name = customer.first_name.clone().unwrap_or_default();
            let (subject, body) = if es {
                (
                    format!("¿Seguimos, {name}? - {}", dealer.name),
                    format!(
                        "Hola {name},\n\nGracias por platicar con nosotros. Si quieres agendar una prueba de manejo o una visita, o tienes otra pregunta, solo responde a este correo o vuelve al chat en nuestro sitio - tu conversación sigue ahí.\n\n{}\n{}\n\n{}\n",
                        dealer.name, dealer.phone.clone().unwrap_or_default(), settings.ai_disclosure_es
                    ),
                )
            } else {
                (
                    format!("Still looking, {name}? - {}", dealer.name),
                    format!(
                        "Hi {name},\n\nThanks for chatting with us. If you'd like to book a test drive or a visit, or you have another question, just reply to this email or come back to the chat on our site - your conversation is right where you left it.\n\n{}\n{}\n\n{}\n",
                        dealer.name, dealer.phone.clone().unwrap_or_default(), settings.ai_disclosure_en
                    ),
                )
            };
            outbox::enqueue(
                &mut tx,
                *dealer_id,
                "followup_email",
                &format!("followup:{convo_id}:v1"),
                serde_json::to_value(OutgoingEmail {
                    to: vec![email.clone()],
                    cc: vec![],
                    from: crate::email::from_address(),
                    subject,
                    text: body,
                    html: None,
                    attachments: vec![],
                    lead_id: None,
                })?,
            )
            .await?;
        }
        tx.commit().await?;
        tracing::info!(conversation = %convo_id, "idle conversation became a lead");
    }
    Ok(idle.len())
}

/// Turns every 'new' lead that has no lead email queued yet into its emails:
/// the human-readable one to the dealership and, if a CRM address is set, the
/// ADF one. Both are queued in one transaction under idempotency keys, so a lead
/// is emailed exactly once no matter how many times this runs.
pub async fn deliver_pending_leads(app: &App) -> Result<usize> {
    let pending: Vec<(Uuid, Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select l.id, l.dealer_id, l.conversation_id, l.customer_id
         from leads l
         where l.status = 'new'
           and not exists (select 1 from outbox o
                           where o.idempotency_key = 'lead:' || l.id || ':v1')
         order by l.created_at
         limit 10",
    )
    .fetch_all(&app.db)
    .await?;

    for (lead_id, dealer_id, convo_id, customer_id) in &pending {
        if let Err(e) = deliver_one(app, *lead_id, *dealer_id, *convo_id, *customer_id).await {
            tracing::error!(lead = %lead_id, "could not assemble lead email: {e:#}");
        }
    }
    Ok(pending.len())
}

async fn deliver_one(
    app: &App,
    lead_id: Uuid,
    dealer_id: Uuid,
    convo_id: Uuid,
    customer_id: Uuid,
) -> Result<()> {
    let settings = db::settings(&app.db, dealer_id).await?;
    let dealer = db::dealer(&app.db, dealer_id).await?;
    let tz = settings.tz();
    let customer = db::customer(&app.db, customer_id).await?;
    let transcript = transcript::build(&app.db, convo_id, dealer_id, tz).await?;

    // The summary is best-effort. If it fails twice the lead still goes out.
    let summary = summary::summarize(
        &app.db,
        &app.llm,
        &app.summary_schema,
        &settings,
        convo_id,
        &transcript,
    )
    .await
    .unwrap_or(summary::Outcome {
        summary: None,
        model: app.llm.model.clone(),
    });
    leads::set_summary(&app.db, lead_id, summary.summary.clone(), &summary.model).await?;

    let shown = db::shown_vehicle_ids(&app.db, convo_id).await?;
    let vehicles = db::vehicles_by_ids(&app.db, dealer_id, &shown).await?;

    let appointment: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        "select a.kind,
                to_char(lower(a.slot) at time zone $2, 'FMDay FMMonth FMDD, FMHH12:MI AM'),
                r.name,
                (select v.year || ' ' || v.make || ' ' || v.model from vehicles v where v.id = a.vehicle_id)
         from appointments a join resources r on r.id = a.resource_id
         where a.lead_id = $1 and a.status = 'confirmed'
         order by lower(a.slot) limit 1",
    )
    .bind(lead_id)
    .bind(&settings.timezone)
    .fetch_optional(&app.db)
    .await?;
    let appointment_text = appointment.as_ref().map(|(kind, when, who, car)| {
        format!(
            "{} on {} {} with {}{}",
            kind.replace('_', " "),
            when,
            settings.timezone,
            who,
            car.as_ref().map(|c| format!(", {c}")).unwrap_or_default()
        )
    });

    let summary_text = summary
        .summary
        .as_ref()
        .map(summary::to_text)
        .unwrap_or_default();

    // Quick yes/no read for the salesperson, derived from the validated summary
    // and the database - the model gets no extra say here.
    let sum = summary.summary.clone().unwrap_or(json!({}));
    let said = |k: &str| sum.pointer(k).map(|v| !v.is_null() && v.as_str() != Some("")).unwrap_or(false);
    let yn = |b: bool| if b { "Yes" } else { "No" };
    let trade = match sum.pointer("/trade_in/has_trade_in").and_then(|v| v.as_bool()) {
        Some(b) => yn(b),
        None => "Not said",
    };
    let facts = vec![
        ("Booked a test drive or visit?", yn(appointment.is_some()).to_string()),
        ("Left a phone number?", yn(customer.phone.is_some()).to_string()),
        ("Left an email?", yn(customer.email.is_some()).to_string()),
        ("Gave a budget?", yn(said("/budget")).to_string()),
        ("Asked about financing?", yn(said("/financing_interest")).to_string()),
        ("Has a trade-in?", trade.to_string()),
        ("Said when they want to buy?", yn(said("/timeline")).to_string()),
    ];
    let language = match customer.preferred_language.as_deref().or(sum["preferred_language"].as_str()) {
        Some("es") => "Spanish",
        _ => "English",
    };

    let name = customer.full_name().unwrap_or_else(|| "unnamed customer".into());
    let headline_car = vehicles.first().map(|v| format!("{} {} {}", v.year, v.make, v.model));
    let subject = match &headline_car {
        Some(car) => format!("New lead: {name}, {car}"),
        None => format!("New lead: {name}"),
    };

    let base_url = std::env::var("PUBLIC_BASE_URL").unwrap_or_default();
    let mut cards = Vec::new();
    for v in &vehicles {
        let photo = db::primary_photo(&app.db, v.id).await?;
        cards.push(json!({
            "label": v.label(),
            "stock": v.stock_number,
            "vin": v.vin,
            "price": v.price(),
            "mileage": v.mileage,
            "condition": v.condition,
            "status": v.status,
            "photo_url": photo.map(|p| format!("{base_url}/{p}")),
            "link": format!("{base_url}/inventory/{}", v.stock_number),
        }));
    }

    let ctx = minijinja::context! {
        dealer => dealer.name,
        name => name,
        phone => customer.phone,
        email => customer.email,
        language => language,
        facts => facts,
        summary_text => summary_text,
        summary_missing => summary.summary.is_none(),
        appointment => appointment_text,
        vehicles => cards,
        transcript => transcript.to_text(),
        lead_id => lead_id.to_string(),
    };
    let html = app.templates.get_template("lead.html")?.render(&ctx)?;
    let text = app.templates.get_template("lead.txt")?.render(&ctx)?;

    let mut to = vec![settings.lead_email.clone()];
    to.extend(settings.notify_emails.iter().cloned());

    let mut tx = app.db.begin().await?;
    outbox::enqueue(
        &mut tx,
        dealer_id,
        "lead_email",
        &format!("lead:{lead_id}:v1"),
        serde_json::to_value(OutgoingEmail {
            to,
            cc: vec![],
            from: crate::email::from_address(),
            subject: subject.clone(),
            text,
            html: Some(html),
            attachments: vec![],
            lead_id: Some(lead_id.to_string()),
        })?,
    )
    .await?;

    if let Some(crm) = &settings.crm_adf_email {
        let mut comments = summary_text.clone();
        if let Some(a) = &appointment_text {
            comments.push_str(&format!("\nAppointment: {a}"));
        }
        if comments.trim().is_empty() {
            comments = "See transcript in the dealership lead email.".into();
        }
        let xml = adf::render(&adf::AdfLead {
            lead_id: lead_id.to_string(),
            requested_at: Utc::now(),
            timezone: tz,
            vehicles: vehicles.clone(),
            first_name: customer.first_name.clone(),
            last_name: customer.last_name.clone(),
            email: customer.email.clone(),
            phone: customer.phone.clone(),
            comments,
            dealer_name: dealer.name.clone(),
        })?;
        outbox::enqueue(
            &mut tx,
            dealer_id,
            "adf_email",
            &format!("adf:{lead_id}:v1"),
            serde_json::to_value(OutgoingEmail {
                to: vec![crm.clone()],
                cc: vec![],
                from: crate::email::from_address(),
                subject,
                // ADF-consuming CRMs read the plain-text body; the attachment
                // is for the ones that parse files instead
                text: xml.clone(),
                html: None,
                attachments: vec![EmailAttachment::from_bytes(
                    "lead.xml",
                    "application/xml",
                    xml.as_bytes(),
                )],
                lead_id: None,
            })?,
        )
        .await?;
    }
    tx.commit().await?;
    tracing::info!(lead = %lead_id, "lead emails queued");
    Ok(())
}

/// The background loop: send queued mail, assemble new leads, sweep for idle
/// conversations. One task, because each job is a query or two and none of them
/// is urgent to the second.
pub fn spawn_worker(app: App) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks: u64 = 0;
        loop {
            if let Err(e) = outbox::tick(&app.db, app.mailer.as_ref()).await {
                tracing::error!("outbox tick failed: {e:#}");
            }
            if let Err(e) = deliver_pending_leads(&app).await {
                tracing::error!("lead delivery failed: {e:#}");
            }
            if ticks % 12 == 0 {
                if let Err(e) = idle_sweep(&app.db).await {
                    tracing::error!("idle sweep failed: {e:#}");
                }
            }
            ticks = ticks.wrapping_add(1);
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_spanish_openers() {
        assert_eq!(detect_language("Hola, busco una camioneta"), "es");
        assert_eq!(detect_language("¿Cuánto cuesta el RAV4?"), "es");
        assert_eq!(detect_language("Hi, looking for an SUV"), "en");
    }

    #[test]
    fn history_merges_consecutive_turns() {
        let mk = |role: &str, text: &str| db::Message {
            id: Uuid::now_v7(),
            role: role.into(),
            content: text.into(),
            tool_calls: None,
            vehicle_ids: vec![],
            created_at: Utc::now(),
        };
        let h = history(&[mk("customer", "hi"), mk("customer", "you there?"), mk("assistant", "yes")]);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].role, "user");
        assert!(h[0].text().contains("you there?"));
    }
}
