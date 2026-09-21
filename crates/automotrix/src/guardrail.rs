//! WHAT: refuses any number the model states that the database cannot back.
//! WHY:  this is the one place where a hallucination costs real money. A bot
//!       that invents "3.9% APR" or "$18,900" has, in Texas, made a statement a
//!       customer can act on - and the summary that repeats it goes into an
//!       email and a CRM. Every other guardrail in this system is about who may
//!       read what; this one is about what may be said.
//! HOW:  build the set of figures the dealer can actually vouch for - every
//!       price, MSRP, mileage and year in inventory, plus every number that
//!       appears in the knowledge-base corpus - then scan the candidate text and
//!       report anything money-shaped that is not in that set. Violations are
//!       logged to guardrail_events and the caller re-asks the model.
//!
//! ponytail: the allowed set is the whole dealer corpus, not just the chunks
//! this conversation retrieved. That means the bot could in principle quote a
//! real figure from an unrelated section. Narrowing it needs retrieved-chunk ids
//! persisted per message; do that if the looseness ever shows up in practice.

use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use sqlx::PgPool;
use std::collections::HashSet;
use std::sync::OnceLock;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Violation {
    /// the text exactly as it appeared
    pub token: String,
    /// normalized form used for the comparison
    pub normalized: String,
    pub reason: String,
}

/// Figures the dealer can stand behind.
#[derive(Debug, Clone, Default)]
pub struct Allowed {
    values: HashSet<String>,
}

impl Allowed {
    pub fn contains(&self, normalized: &str) -> bool {
        self.values.contains(normalized)
    }

    pub fn insert_raw(&mut self, s: &str) {
        for n in extract_numbers(s) {
            self.values.insert(n);
        }
    }

    pub fn insert_cents(&mut self, cents: i64) {
        // a price may legitimately be written "$24,950" or "24950.00"
        self.values.insert(normalize_number(&format!("{}", cents / 100)));
        self.values
            .insert(normalize_number(&format!("{}.{:02}", cents / 100, cents % 100)));
    }

    pub fn insert_int(&mut self, n: i64) {
        self.values.insert(normalize_number(&n.to_string()));
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Every figure in this dealer's inventory and knowledge base.
pub async fn allowed_for_dealer(db: &PgPool, dealer_id: Uuid) -> Result<Allowed> {
    let mut allowed = Allowed::default();

    let rows: Vec<(i64, Option<i64>, i32, i16)> = sqlx::query_as(
        "select list_price_cents, msrp_cents, mileage, year from vehicles where dealer_id = $1",
    )
    .bind(dealer_id)
    .fetch_all(db)
    .await?;
    for (price, msrp, mileage, year) in rows {
        allowed.insert_cents(price);
        if let Some(m) = msrp {
            allowed.insert_cents(m);
        }
        allowed.insert_int(mileage as i64);
        allowed.insert_int(year as i64);
    }

    // every number the knowledge base itself states - APR ranges, fees, warranty
    // terms, hours. If it is not in the corpus and not in inventory, the bot has
    // no business saying it.
    let chunks: Vec<(String,)> =
        sqlx::query_as("select content from doc_chunks where dealer_id = $1")
            .bind(dealer_id)
            .fetch_all(db)
            .await?;
    for (content,) in chunks {
        allowed.insert_raw(&content);
    }

    Ok(allowed)
}

fn money_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // $12,345.67 | $1200 | 12,345 | 4-or-more bare digits | 6.5%
    RE.get_or_init(|| {
        Regex::new(r"\$\s?\d[\d,]*(?:\.\d+)?|\b\d[\d,]*\.\d+\s?%|\b\d[\d,]*\s?%|\b\d{1,3}(?:,\d{3})+(?:\.\d+)?\b|\b\d{4,}(?:\.\d+)?\b")
            .expect("static regex")
    })
}

/// Phone numbers, email addresses, ids, VINs and stock numbers all contain digit
/// runs that are not claims about money. Without stripping them first, every
/// reply that repeats a customer's phone number trips the guardrail - and every
/// summary containing a phone fails validation twice and ships with no summary.
fn not_a_figure_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
            r"|\+\d[\d\s().-]{7,18}\d",
            r"|\(?\b\d{3}\)?[\s.-]\d{3}[\s.-]\d{4}\b",
            r"|\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
            r"|\b[A-HJ-NPR-Z0-9]{17}\b",
            r"|\b[A-Z]{1,4}\d{2}-\d{3,5}\b",
        ))
        .expect("static regex")
    })
}

fn number_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d[\d,]*(?:\.\d+)?").expect("static regex"))
}

/// "$24,950.00" -> "24950", "6.50%" -> "6.5". Trailing zeros after the decimal
/// point are dropped so the same figure written two ways compares equal.
pub fn normalize_number(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let (int, frac) = match cleaned.split_once('.') {
        Some((i, f)) => (i, f.trim_end_matches('0')),
        None => (cleaned.as_str(), ""),
    };
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    if frac.is_empty() {
        int.to_string()
    } else {
        format!("{int}.{frac}")
    }
}

fn extract_numbers(text: &str) -> Vec<String> {
    number_re()
        .find_iter(text)
        .map(|m| normalize_number(m.as_str()))
        .filter(|n| !n.is_empty())
        .collect()
}

/// Years, small counts and clock times are not claims about money; checking them
/// only produces noise. A bare number under this threshold with no `$` or `%` is
/// left alone.
const BARE_NUMBER_FLOOR: f64 = 1000.0;

pub fn check(text: &str, allowed: &Allowed) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let text = not_a_figure_re().replace_all(text, " ");

    for m in money_re().find_iter(&text) {
        let token = m.as_str().trim().to_string();
        let normalized = normalize_number(&token);
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            continue;
        }

        let is_money = token.contains('$');
        let is_percent = token.contains('%');
        if !is_money && !is_percent {
            // a bare number: only care if it is big enough to be a price or a
            // mileage, and not a plausible model year
            let value: f64 = normalized.parse().unwrap_or(0.0);
            if value < BARE_NUMBER_FLOOR {
                continue;
            }
            if (1990.0..=2030.0).contains(&value) && !token.contains(',') {
                continue;
            }
        }

        if !allowed.contains(&normalized) {
            out.push(Violation {
                token: token.clone(),
                normalized,
                reason: if is_percent {
                    "rate not present in the knowledge base".to_string()
                } else {
                    "figure not found in inventory or the knowledge base".to_string()
                },
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed_with(items: &[&str]) -> Allowed {
        let mut a = Allowed::default();
        for i in items {
            a.insert_raw(i);
        }
        a
    }

    #[test]
    fn normalizes_equivalent_spellings() {
        assert_eq!(normalize_number("$24,950"), "24950");
        assert_eq!(normalize_number("24950.00"), "24950");
        assert_eq!(normalize_number("6.50%"), "6.5");
        assert_eq!(normalize_number("0"), "0");
    }

    #[test]
    fn accepts_a_price_that_is_in_inventory() {
        let mut allowed = Allowed::default();
        allowed.insert_cents(2495000);
        assert!(check("It's listed at $24,950 today.", &allowed).is_empty());
    }

    #[test]
    fn rejects_an_invented_price() {
        let mut allowed = Allowed::default();
        allowed.insert_cents(2495000);
        let v = check("I can do $18,900 for you.", &allowed);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].normalized, "18900");
    }

    #[test]
    fn rejects_an_invented_rate() {
        let allowed = allowed_with(&["Prime 661-780 6.0% - 8.5%"]);
        let v = check("You'd get 3.9% APR.", &allowed);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].token, "3.9%");
    }

    #[test]
    fn accepts_a_rate_the_knowledge_base_states() {
        let allowed = allowed_with(&["Prime 661-780 6.0% - 8.5%"]);
        assert!(check("Prime buyers usually see 6.0% to 8.5%.", &allowed).is_empty());
    }

    #[test]
    fn ignores_years_and_small_counts() {
        let allowed = Allowed::default();
        assert!(check("The 2021 RAV4 seats 5 and has 2 keys.", &allowed).is_empty());
    }

    #[test]
    fn phones_emails_and_ids_are_not_figures() {
        let allowed = Allowed::default();
        for text in [
            "I'll text you at +12105550143.",
            "Call us at (210) 555-0142.",
            "Saved maria.2024@example.com for you.",
            "Lead 0199a1b2-c3d4-7e5f-8a90-1b2c3d4e5f60 created.",
            "VIN JTMRWRFV8LD072316, stock AX21-0061.",
        ] {
            assert!(check(text, &allowed).is_empty(), "false positive on: {text}");
        }
    }

    #[test]
    fn stripping_does_not_hide_a_real_price_next_to_a_phone() {
        let allowed = Allowed::default();
        let v = check("Call +12105550143 and ask for the $18,900 deal.", &allowed);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].normalized, "18900");
    }

    #[test]
    fn catches_an_invented_mileage() {
        let mut allowed = Allowed::default();
        allowed.insert_int(44761);
        let v = check("It has 31,200 miles.", &allowed);
        assert_eq!(v.len(), 1);
    }
}
