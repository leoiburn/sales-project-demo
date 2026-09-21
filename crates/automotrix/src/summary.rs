//! WHAT: turns a transcript into a structured lead summary, then distrusts it.
//! WHY:  a salesperson reads the summary first, so it has to be right. It is
//!       also the one place the model writes prose that leaves the building,
//!       which makes it the likeliest place for an invented price to escape.
//! HOW:  one Claude call returns JSON against a schema loaded from
//!       config/summary_schemas/<vertical>.json - the pipeline is vertical-
//!       agnostic, so a law-firm version is a new file, not new code. Then code
//!       validates it:
//!         1. vehicles never shown in this conversation are dropped
//!         2. any field with no evidence, or evidence pointing at a message that
//!            is not in this conversation, is nulled
//!         3. the price/number guardrail runs over the whole summary
//!       Two failed attempts and the lead goes out with the transcript alone.
//!       A lead must never be lost because a summary was bad.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::db::{self, DealerSettings};
use crate::guardrail;
use crate::llm::{self, Message};
use crate::transcript::Transcript;

pub fn load_schema(vertical: &str) -> Result<Value> {
    let path = crate::path(&format!("config/summary_schemas/{vertical}.json"));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("missing summary schema {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

/// The transcript as the summarizer sees it: every line tagged with its message
/// id, so the model can cite evidence and code can check the citation.
fn transcript_for_model(t: &Transcript) -> String {
    let mut out = String::new();
    for line in &t.lines {
        out.push_str(&format!("[{}] {}: {}\n", line.message_id, line.speaker, line.text));
        for shown in &line.shown {
            out.push_str(&format!("    [Shown: {shown}]\n"));
        }
    }
    out
}

/// Fields whose evidence is checked. Keys match the schema's `evidence` object.
const FIELDS: [&str; 10] = [
    "customer_name", "contact", "preferred_language", "vehicles_of_interest",
    "budget", "trade_in", "financing_interest", "timeline", "open_questions", "next_step",
];

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::String(s) => s.trim().is_empty(),
        Value::Object(o) => o.values().all(is_empty),
        _ => false,
    }
}

fn null_of(v: &Value) -> Value {
    match v {
        Value::Array(_) => json!([]),
        Value::Object(o) => Value::Object(o.keys().map(|k| (k.clone(), Value::Null)).collect()),
        _ => Value::Null,
    }
}

#[derive(Debug, Default)]
pub struct Validation {
    pub summary: Value,
    pub dropped_vehicles: Vec<String>,
    pub dropped_fields: Vec<String>,
    pub guardrail: Vec<guardrail::Violation>,
}

/// Fields the model writes in its own words. Any figure in them is a claim by
/// the dealership, so it must be backed by inventory or the knowledge base.
const AUTHORED: [&str; 4] = ["financing_interest", "timeline", "open_questions", "next_step"];

/// Fields copied verbatim from the customer ("my budget is $25k"). A figure here
/// is the customer's, not the dealer's - checking it against inventory would
/// reject every lead with a stated budget. Instead it must literally appear in
/// the customer message the field cites, which is what stops the model from
/// laundering an invented number through a "quote".
const VERBATIM: [&str; 2] = ["/budget", "/trade_in/details"];

/// Pure function over the model's output, so it is testable without a model.
/// `messages` maps message id to its text, for the evidence checks.
pub fn validate(
    raw: &Value,
    shown_stock_numbers: &HashSet<String>,
    messages: &HashMap<String, String>,
    allowed: &guardrail::Allowed,
) -> Validation {
    let mut s = raw.clone();
    let mut v = Validation::default();

    // 1. a vehicle may only be of interest if it was actually shown
    if let Some(arr) = s.get_mut("vehicles_of_interest").and_then(|x| x.as_array_mut()) {
        arr.retain(|stock| {
            let keep = stock.as_str().map(|s| shown_stock_numbers.contains(s)).unwrap_or(false);
            if !keep {
                v.dropped_vehicles.push(stock.to_string());
            }
            keep
        });
    }

    // 2. no evidence, no field
    let evidence = s.get("evidence").cloned().unwrap_or(json!({}));
    for field in FIELDS {
        let Some(value) = s.get(field).cloned() else { continue };
        if is_empty(&value) {
            continue;
        }
        let cited: Vec<String> = evidence
            .get(field)
            .and_then(|e| e.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let supported = !cited.is_empty() && cited.iter().all(|id| messages.contains_key(id));
        if !supported {
            v.dropped_fields.push(field.to_string());
            s[field] = null_of(&value);
        }
    }

    // 3a. what the model wrote in its own words: backed by the dealer's data
    let authored: String = AUTHORED
        .iter()
        .filter_map(|f| s.get(*f))
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    v.guardrail = guardrail::check(&authored, allowed);

    // 3b. what the model says the customer said: the figure has to be in the
    // message it cites, verbatim
    for pointer in VERBATIM {
        let Some(text) = s.pointer(pointer).and_then(|x| x.as_str()).map(String::from) else {
            continue;
        };
        let field = pointer.trim_start_matches('/').split('/').next().unwrap_or_default();
        let mut quoted = guardrail::Allowed::default();
        for id in evidence
            .get(field)
            .and_then(|e| e.as_array())
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str())
        {
            if let Some(body) = messages.get(id) {
                quoted.insert_raw(body);
            }
        }
        v.guardrail.extend(guardrail::check(&text, &quoted));
    }

    v.summary = s;
    v
}

pub struct Outcome {
    pub summary: Option<Value>,
    pub model: String,
}

pub async fn summarize(
    db: &PgPool,
    llm: &llm::Client,
    schema_doc: &Value,
    settings: &DealerSettings,
    conversation_id: Uuid,
    transcript: &Transcript,
) -> Result<Outcome> {
    let schema = schema_doc.get("schema").cloned().unwrap_or(json!({}));
    let instructions = schema_doc
        .get("instructions")
        .and_then(|x| x.as_str())
        .unwrap_or("Summarize the conversation.");
    let language = if settings.summary_language == "es" { "Spanish" } else { "English" };

    let system = format!(
        "{instructions}\n\nWrite every free-text field in {language}, except budget and trade-in details, which stay verbatim in whatever language the customer used. Respond with a single JSON object only."
    );

    let messages: HashMap<String, String> = transcript
        .lines
        .iter()
        .map(|l| (l.message_id.to_string(), l.text.clone()))
        .collect();
    let shown_ids = db::shown_vehicle_ids(db, conversation_id).await?;
    let shown_stock: HashSet<String> = db::vehicles_by_ids(db, settings.dealer_id, &shown_ids)
        .await?
        .into_iter()
        .map(|v| v.stock_number)
        .collect();
    let allowed = guardrail::allowed_for_dealer(db, settings.dealer_id).await?;

    let prompt = vec![Message::user_text(format!(
        "Transcript:\n\n{}",
        transcript_for_model(transcript)
    ))];

    for attempt in 1..=2 {
        let raw = match llm.complete_json(&system, &prompt, &schema, 2000).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(attempt, "summary call failed: {e}");
                continue;
            }
        };
        let v = validate(&raw, &shown_stock, &messages, &allowed);

        if !v.dropped_vehicles.is_empty() || !v.dropped_fields.is_empty() {
            db::record_guardrail_event(
                db,
                Some(conversation_id),
                settings.dealer_id,
                "summary_unsupported_claims",
                json!({ "attempt": attempt, "dropped_vehicles": v.dropped_vehicles,
                        "dropped_fields": v.dropped_fields }),
            )
            .await?;
        }

        if v.guardrail.is_empty() {
            return Ok(Outcome {
                summary: Some(v.summary),
                model: llm.model.clone(),
            });
        }

        db::record_guardrail_event(
            db,
            Some(conversation_id),
            settings.dealer_id,
            "summary_unbacked_figure",
            json!({ "attempt": attempt, "violations": v.guardrail }),
        )
        .await?;
    }

    // Two strikes. Ship the lead anyway, transcript only.
    tracing::warn!(%conversation_id, "summary failed validation twice; sending transcript only");
    Ok(Outcome {
        summary: None,
        model: llm.model.clone(),
    })
}

/// A readable rendering of a validated summary, for the email body and the ADF
/// comments. Built from the JSON, never asked of the model.
pub fn to_text(summary: &Value) -> String {
    let mut out = String::new();
    let mut line = |label: &str, value: Option<String>| {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            out.push_str(&format!("{label}: {v}\n"));
        }
    };
    let s = |k: &str| summary.get(k).and_then(|v| v.as_str()).map(String::from);

    line("Name", s("customer_name"));
    line("Phone", summary.pointer("/contact/phone").and_then(|v| v.as_str()).map(String::from));
    line("Email", summary.pointer("/contact/email").and_then(|v| v.as_str()).map(String::from));
    line("Language", s("preferred_language"));
    line(
        "Interested in",
        summary.get("vehicles_of_interest").and_then(|v| v.as_array()).map(|a| {
            a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")
        }),
    );
    line("Budget", s("budget"));
    if summary.pointer("/trade_in/has_trade_in").and_then(|v| v.as_bool()) == Some(true) {
        line(
            "Trade-in",
            Some(
                summary
                    .pointer("/trade_in/details")
                    .and_then(|v| v.as_str())
                    .unwrap_or("yes")
                    .to_string(),
            ),
        );
    }
    line("Financing", s("financing_interest"));
    line("Timeline", s("timeline"));
    line(
        "Open questions",
        summary.get("open_questions").and_then(|v| v.as_array()).map(|a| {
            a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; ")
        }),
    );
    line("Next step", s("next_step"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// m1: the customer's opener, m2: their phone, m3: the assistant showing a car
    fn msgs() -> HashMap<String, String> {
        [
            ("m1", "Hola soy María, busco algo de menos de 25 mil, en español porfa"),
            ("m2", "mi cel es 210 555 0143"),
            ("m3", "Here is the RAV4, stock AX21-0061"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    fn raw() -> Value {
        json!({
            "customer_name": "María",
            "contact": {"phone": "+12105550143", "email": null},
            "preferred_language": "es",
            "vehicles_of_interest": ["AX21-0061", "AX99-9999"],
            "budget": "menos de 25 mil",
            "trade_in": {"has_trade_in": null, "details": null},
            "financing_interest": null,
            "timeline": "this weekend",
            "open_questions": [],
            "next_step": "test drive",
            "evidence": {
                "customer_name": ["m1"], "contact": ["m2"], "preferred_language": ["m1"],
                "vehicles_of_interest": ["m3"], "budget": ["m1"], "trade_in": [],
                "financing_interest": [], "timeline": [], "open_questions": [],
                "next_step": ["m-does-not-exist"]
            }
        })
    }

    #[test]
    fn drops_vehicles_that_were_never_shown() {
        let v = validate(&raw(), &ids(&["AX21-0061"]), &msgs(), &guardrail::Allowed::default());
        assert_eq!(v.summary["vehicles_of_interest"], json!(["AX21-0061"]));
        assert_eq!(v.dropped_vehicles.len(), 1);
    }

    #[test]
    fn drops_fields_without_evidence() {
        let v = validate(&raw(), &ids(&["AX21-0061"]), &msgs(), &guardrail::Allowed::default());
        // timeline has a value but cites nothing
        assert_eq!(v.summary["timeline"], Value::Null);
        // next_step cites a message that is not in this conversation
        assert_eq!(v.summary["next_step"], Value::Null);
        // budget is properly cited and survives, verbatim
        assert_eq!(v.summary["budget"], json!("menos de 25 mil"));
        assert!(v.dropped_fields.contains(&"timeline".to_string()));
        assert!(v.dropped_fields.contains(&"next_step".to_string()));
    }

    #[test]
    fn guardrail_catches_a_figure_in_the_summary() {
        let mut r = raw();
        r["next_step"] = json!("offer $18,900 out the door");
        r["evidence"]["next_step"] = json!(["m1"]);
        let v = validate(&r, &ids(&["AX21-0061"]), &msgs(), &guardrail::Allowed::default());
        assert_eq!(v.guardrail.len(), 1);
        assert_eq!(v.guardrail[0].normalized, "18900");
    }

    #[test]
    fn a_phone_number_does_not_sink_the_summary() {
        // regression: the guardrail used to read the phone as an unbacked
        // figure, so every summary with contact details failed twice and the
        // lead shipped without one
        let v = validate(&raw(), &ids(&["AX21-0061"]), &msgs(), &guardrail::Allowed::default());
        assert!(v.guardrail.is_empty(), "unexpected: {:?}", v.guardrail);
    }

    #[test]
    fn a_customer_budget_is_checked_against_what_they_said() {
        // the customer said "$25,000"; the dealer database knows nothing about
        // it, and it must still pass - it is a quote, not a claim
        let mut m = msgs();
        m.insert("m1".into(), "my budget is $25,000".into());
        let mut r = raw();
        r["budget"] = json!("$25,000");
        let v = validate(&r, &ids(&["AX21-0061"]), &m, &guardrail::Allowed::default());
        assert!(v.guardrail.is_empty(), "unexpected: {:?}", v.guardrail);
    }

    #[test]
    fn an_invented_budget_is_caught_even_though_it_is_labelled_a_quote() {
        // the customer never said $32,000; the model cannot launder it in as
        // "what the customer said"
        let mut m = msgs();
        m.insert("m1".into(), "my budget is $25,000".into());
        let mut r = raw();
        r["budget"] = json!("$32,000");
        let v = validate(&r, &ids(&["AX21-0061"]), &m, &guardrail::Allowed::default());
        assert_eq!(v.guardrail.len(), 1);
        assert_eq!(v.guardrail[0].normalized, "32000");
    }

    #[test]
    fn the_shipped_schema_loads_and_is_strict() {
        let doc = load_schema("dealership").unwrap();
        assert_eq!(doc["schema"]["additionalProperties"], json!(false));
        let required: Vec<&str> = doc["schema"]["required"].as_array().unwrap()
            .iter().filter_map(|x| x.as_str()).collect();
        for f in FIELDS {
            assert!(required.contains(&f), "{f} missing from schema");
        }
    }
}
