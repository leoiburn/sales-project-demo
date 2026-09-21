//! WHAT: chunks and embeds every RAG document, writing seed/corpus.ndjson.
//! WHY:  the bot answers policy and product questions from this corpus. It never
//!       answers inventory questions from it - those are SQL over vehicles. Two
//!       document sets go in:
//!         1. the Automotrix knowledge base (policy, financing, warranty, FAQ)
//!         2. per-model sales notes (strengths, objections, talk tracks)
//! HOW:  split on section headings, embed with bge-base-en-v1.5 (768 dims,
//!       CLS pooling, L2-normalized so cosine is the matching metric) through
//!       fastembed's ONNX build of the model, and tag every chunk with the risk
//!       metadata the guardrail layer needs.
//!
//!       The model is downloaded once (~440MB) into .fastembed_cache/.
//!
//!   cargo run --release -p datagen --bin build_corpus

use anyhow::{bail, Result};
use datagen::{path, sha256_hex, spec_paths};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Recorded on every chunk. fastembed serves the ONNX export of this exact model
/// (Xenova/bge-base-en-v1.5); the weights are BAAI's.
const MODEL_NAME: &str = "BAAI/bge-base-en-v1.5";
const DIM: usize = 768;
const MAX_TOKENS: usize = 512;
const KB_PATH: &str = "seed/knowledge_base/automotrix_knowledge_base.txt";

/// Legal exposure if the bot states this wrong. 'high' = money, contract or a
/// regulated disclosure (TILA, Texas DTPA, Magnuson-Moss, FTC Used Car Rule);
/// those chunks may never be answered without their disclaimer attached.
fn kb_risk(section: u32) -> &'static str {
    match section {
        1 | 2 => "low",
        3 | 8 | 14 => "medium",
        4 | 5 | 6 | 7 | 9 | 10 | 11 | 12 | 13 | 15 => "high",
        _ => "low",
    }
}

fn kb_disclaimer(section: u32) -> Option<&'static str> {
    Some(match section {
        4 => "Estimate only, not an offer of credit. All rates and approvals are subject to lender review. Confirm with a finance associate.",
        5 => "Tax and fee amounts are estimates and vary by county and by date. Confirm the final figure with a sales associate.",
        6 => "Trade values require an in-person appraisal. No figure quoted here is an offer.",
        7 => "Deposit terms are set by the signed deposit agreement, not by this chat.",
        9 => "Coverage depends on the specific vehicle and the FTC Buyers Guide posted on its window. Verify before purchase.",
        10 => "Texas has no cooling-off period. Exchange terms are set by the signed agreement. Confirm eligibility with a manager.",
        11 => "Optional products. Terms, cancellation and refunds are governed by the product contract.",
        12 => "General warranty summary only, not a legal opinion or a coverage determination. Coverage follows the VIN. For Lemon Law questions contact the customer relations manager at (210) 555-0142.",
        13 => "Service prices are estimates and exclude tax. A written estimate is provided before any work begins.",
        15 => "Summary answers only. For exact figures talk to an associate.",
        _ => return None,
    })
}

const SPEC_DISCLAIMER: &str = "Model-level information, not a specific vehicle. Equipment, price and availability vary by unit - confirm against the stock number in inventory.";
const PAYMENT_DISCLAIMER: &str = "Illustrative payment example only. Not an offer of credit and not a quote. Actual terms depend on lender approval, taxes and fees.";

/// Per-model sales notes: each key becomes one chunk.
const SPEC_PARTS: &[(&str, &str, &str)] = &[
    ("strengths", "Strengths", "low"),
    ("weaknesses", "Weaknesses", "low"),
    ("ideal_buyer", "Ideal buyer", "low"),
    ("use_cases", "Use cases", "low"),
    ("competitors", "Cross-shopped against", "low"),
    ("sales_objections", "Objection handling", "medium"),
    ("talk_tracks", "Test drive and walkaround", "low"),
    ("ownership", "Ownership costs", "medium"),
    ("warranty", "Warranty", "high"),
    ("safety", "Safety", "medium"),
    ("financing_example", "Financing example", "high"),
];

fn chunk(
    index: usize,
    heading: String,
    content: String,
    audience: &str,
    risk: &str,
    disclaimer: Option<&str>,
    metadata: Value,
) -> Value {
    json!({
        "chunk_index": index, "heading": heading, "content": content,
        "audience": audience, "risk": risk, "disclaimer": disclaimer, "metadata": metadata,
    })
}

/// Splits a section before each numbered subsection ("4.1 ", "4.2 "...),
/// dropping the newline and keeping the number. The regex crate has no
/// lookahead, so the split points are found and cut by hand.
fn split_subsections(section: &str) -> Vec<String> {
    let re = Regex::new(r"\n\d+\.\d+ ").unwrap();
    let mut out = Vec::new();
    let mut start = 0;
    for m in re.find_iter(section) {
        out.push(section[start..m.start()].to_string());
        start = m.start() + 1; // skip the newline, keep "4.1 ..."
    }
    out.push(section[start..].to_string());
    out
}

fn chunk_kb(raw: &str) -> Vec<Value> {
    let raw = raw.split("END OF DOCUMENT").next().unwrap_or(raw);
    let sep = Regex::new(r"\n-+\nSECTION ").unwrap();
    let sections: Vec<String> = sep
        .split(raw)
        .skip(1)
        .map(|p| format!("SECTION {p}").trim().to_string())
        .collect();

    let mut pieces: Vec<String> = Vec::new();
    for sec in &sections {
        let title = sec.lines().next().unwrap_or_default();
        let subs = split_subsections(sec);
        if sec.chars().count() < 1500 || subs.len() == 1 {
            pieces.push(sec.clone());
        } else {
            pieces.push(subs[0].clone());
            pieces.extend(subs[1..].iter().map(|s| format!("{title}\n{s}")));
        }
    }
    pieces.retain(|p| p.trim().chars().count() > 100);

    let num_re = Regex::new(r"SECTION (\d+)").unwrap();
    let sub_re = Regex::new(r"(?m)^(\d+\.\d+)").unwrap();
    pieces
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let num: u32 = num_re.captures(&c).map(|m| m[1].parse().unwrap_or(0)).unwrap_or(0);
            let next_two: String = c.lines().skip(1).take(2).collect::<Vec<_>>().join("\n");
            let sub = sub_re.captures(&next_two).map(|m| m[1].to_string());
            let first = c.lines().next().unwrap_or_default().trim().to_string();
            let heading = match &sub {
                Some(s) => format!("{first} / {s}"),
                None => first,
            };
            // Section 16 is the bot's own operating rules. It belongs in the
            // system prompt, never in a customer-facing answer.
            let audience = if num == 16 { "system" } else { "customer" };
            chunk(i, heading, c.clone(), audience, kb_risk(num), kb_disclaimer(num),
                  json!({ "section": num, "subsection": sub }))
        })
        .collect()
}

/// Scalars as plain text: strings without quotes, everything else as JSON.
fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn render(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            if matches!(items.first(), Some(Value::Object(_))) {
                items
                    .iter()
                    .map(|item| {
                        let parts: Vec<String> = item
                            .as_object()
                            .map(|o| o.iter().map(|(k, v)| format!("{k}: {}", scalar(v))).collect())
                            .unwrap_or_default();
                        format!("- {}", parts.join("; "))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                items.iter().map(|v| format!("- {}", scalar(v))).collect::<Vec<_>>().join("\n")
            }
        }
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| {
                let k = k.replace('_', " ");
                if v.is_array() || v.is_object() {
                    format!("- {k}:\n{}", render(v))
                } else {
                    format!("- {k}: {}", render(v))
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => scalar(other),
    }
}

fn chunk_specs(spec: &Value) -> Vec<Value> {
    let label = format!(
        "{} {} {}",
        spec["model_year"], scalar(&spec["make"]), scalar(&spec["model"])
    );
    let mut out = Vec::new();
    for (key, heading, risk) in SPEC_PARTS {
        let Some(v) = spec.get(*key) else { continue };
        let body = render(v).trim().to_string();
        if body.chars().count() < 40 {
            continue;
        }
        let disclaimer = if *key == "financing_example" {
            Some(PAYMENT_DISCLAIMER)
        } else if *risk == "high" {
            Some(SPEC_DISCLAIMER)
        } else {
            None
        };
        out.push(chunk(
            out.len(),
            format!("{label} - {heading}"),
            format!("{label} - {heading}\n{body}"),
            "customer",
            risk,
            disclaimer,
            json!({ "catalog_id": spec["id"], "make": spec["make"], "model": spec["model"],
                    "model_year": spec["model_year"], "part": key }),
        ));
    }
    out
}

fn main() -> Result<()> {
    let mut docs: Vec<Map<String, Value>> = Vec::new();

    let kb_raw = std::fs::read_to_string(path(KB_PATH))?;
    let mut kb = Map::new();
    kb.insert("doc_key".into(), json!("automotrix-kb"));
    kb.insert("title".into(), json!("Automotrix - Customer Knowledge Base"));
    kb.insert("kind".into(), json!("policy"));
    kb.insert("source_path".into(), json!(KB_PATH));
    kb.insert("body".into(), json!(kb_raw));
    kb.insert("chunks".into(), Value::Array(chunk_kb(&kb_raw)));
    docs.push(kb);

    for spec_path in spec_paths()? {
        let spec: Value = serde_json::from_str(&std::fs::read_to_string(&spec_path)?)?;
        let rel = spec_path.strip_prefix(datagen::repo_root()).unwrap_or(&spec_path);
        let mut d = Map::new();
        d.insert("doc_key".into(), json!(format!("specs-{}", scalar(&spec["id"]))));
        d.insert("title".into(), json!(format!(
            "{} {} {} - sales notes", spec["model_year"], scalar(&spec["make"]), scalar(&spec["model"]))));
        d.insert("kind".into(), json!("specs"));
        d.insert("source_path".into(), json!(rel.display().to_string()));
        d.insert("body".into(), json!(serde_json::to_string_pretty(&spec)?));
        d.insert("chunks".into(), Value::Array(chunk_specs(&spec)));
        docs.push(d);
    }

    let mut model = TextEmbedding::try_new(
        TextInitOptions::new(EmbeddingModel::BGEBaseENV15)
            .with_max_length(MAX_TOKENS)
            .with_cache_dir(path(".fastembed_cache"))
            .with_show_download_progress(true),
    )?;

    // fastembed's tokenizer truncates and pads to MAX_TOKENS, so a count taken
    // through it could never exceed the limit and the overflow check below would
    // be decorative. Count with an unconfigured copy instead.
    let mut counter = model.tokenizer.clone();
    counter.with_padding(None);
    counter
        .with_truncation(None)
        .map_err(|e| anyhow::anyhow!("tokenizer: {e}"))?;
    let count = |text: &str| -> Result<usize> {
        Ok(counter
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("tokenizer: {e}"))?
            .len())
    };

    let texts: Vec<String> = docs
        .iter()
        .flat_map(|d| d["chunks"].as_array().unwrap().iter())
        .map(|c| c["content"].as_str().unwrap().to_string())
        .collect();

    let mut truncated = 0;
    let mut token_counts = Vec::with_capacity(texts.len());
    for t in &texts {
        let n = count(t)?;
        if n > MAX_TOKENS {
            truncated += 1;
            println!("AVISO: chunk de {n} tokens, se truncara a {MAX_TOKENS}: {}",
                     t.lines().next().unwrap_or_default());
        }
        token_counts.push(n);
    }

    let vectors = model.embed(&texts, Some(32))?;
    if vectors.len() != texts.len() || vectors.iter().any(|v| v.len() != DIM) {
        bail!("expected {} vectors of {DIM} dims", texts.len());
    }

    // L2-normalize explicitly: cosine distance in pgvector assumes it, and the
    // self-retrieval check in `verify` depends on it
    let mut i = 0;
    for d in docs.iter_mut() {
        let chunks = d.get_mut("chunks").unwrap().as_array_mut().unwrap();
        for c in chunks.iter_mut() {
            let v = &vectors[i];
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(f32::EPSILON);
            let rounded: Vec<f64> = v
                .iter()
                .map(|x| ((*x / norm) as f64 * 1e6).round() / 1e6)
                .collect();
            let content = c["content"].as_str().unwrap().to_string();
            let o = c.as_object_mut().unwrap();
            o.insert("embedding".into(), json!(rounded));
            o.insert("embedding_model".into(), json!(MODEL_NAME));
            o.insert("token_count".into(), json!(token_counts[i]));
            o.insert("content_hash".into(), json!(sha256_hex(&content)));
            i += 1;
        }
        let body_hash = sha256_hex(d["body"].as_str().unwrap());
        d.insert("content_hash".into(), json!(body_hash));
    }

    let mut out = String::new();
    for d in &docs {
        out.push_str(&serde_json::to_string(d)?);
        out.push('\n');
    }
    std::fs::write(path("seed/corpus.ndjson"), out)?;

    let all: Vec<&Value> = docs.iter().flat_map(|d| d["chunks"].as_array().unwrap().iter()).collect();
    let mut risks: BTreeMap<&str, usize> = BTreeMap::new();
    for c in all.iter().filter(|c| c["audience"] == "customer") {
        *risks.entry(c["risk"].as_str().unwrap()).or_default() += 1;
    }
    let naked = all.iter().filter(|c| c["risk"] == "high" && c["disclaimer"].is_null()).count();
    println!("documentos: {}  chunks: {}  dim: {DIM}", docs.len(), all.len());
    println!("riesgo (customer): {risks:?} | system: {}",
             all.iter().filter(|c| c["audience"] == "system").count());
    println!("truncados: {truncated}");
    if naked > 0 {
        bail!("{naked} high-risk chunks without a disclaimer");
    }
    println!("high-risk sin disclaimer: 0");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_before_numbered_subsections_keeping_the_number() {
        let parts = split_subsections("SECTION 4: FINANCING\n4.1 GENERAL\nx\n4.2 RATES\ny");
        assert_eq!(parts, vec!["SECTION 4: FINANCING", "4.1 GENERAL\nx", "4.2 RATES\ny"]);
    }

    #[test]
    fn section_16_is_system_only() {
        let raw = format!(
            "intro\n-----\nSECTION 15: FAQ\n{}\n-----\nSECTION 16: ASSISTANT GUIDELINES\n{}\nEND OF DOCUMENT",
            "Q: a question long enough to keep. ".repeat(5),
            "- Never guarantee credit approval, it is not ours to promise. ".repeat(3)
        );
        let chunks = chunk_kb(&raw);
        let s16 = chunks.iter().find(|c| c["metadata"]["section"] == 16).unwrap();
        assert_eq!(s16["audience"], "system");
        let s15 = chunks.iter().find(|c| c["metadata"]["section"] == 15).unwrap();
        assert_eq!(s15["audience"], "customer");
        assert!(s15["disclaimer"].is_string(), "section 15 is high risk and needs its disclaimer");
    }

    #[test]
    fn renders_nested_specs_readably() {
        let v = json!({ "basic_warranty": "3 yr", "notes": ["a", "b"] });
        assert_eq!(render(&v), "- basic warranty: 3 yr\n- notes:\n- a\n- b");
    }
}
