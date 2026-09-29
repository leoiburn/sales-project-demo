//! WHAT: renders cars/<slug>/README.md, the root catalog.json and README.md from
//!       each cars/<slug>/specs.json.
//! WHY:  specs.json is the source of truth; everything else here is generated so
//!       the human-readable and machine-readable copies never disagree.
//! HOW:  one pass over the specs, sorted by folder name. README.md is
//!       README.template.md with {{CATALOG_TABLE}} filled in.
//!
//!   cargo run -p datagen --bin build_docs

use anyhow::{Context, Result};
use datagen::{path, spec_paths, thousands};
use serde_json::{json, Map, Value};
use std::fmt::Write as _;
use std::fs;

fn money(v: &Value) -> String {
    match v.as_f64() {
        Some(n) => format!("${}", thousands(n.round() as i64)),
        None => show(v),
    }
}

/// "epa_mpg" -> "Epa mpg": underscores to spaces, first letter up, rest down.
fn title(k: &str) -> String {
    let s = k.replace('_', " ").to_lowercase();
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Scalar as the docs print it: strings bare, booleans as True/False.
fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

fn kv_table(d: &Value, skip: &[&str]) -> String {
    let mut rows = vec!["| Field | Value |".to_string(), "|---|---|".to_string()];
    for (k, v) in d.as_object().into_iter().flatten() {
        if skip.contains(&k.as_str()) || v.is_null() {
            continue;
        }
        let cell = match v {
            Value::Object(o) => o
                .iter()
                .filter(|(_, b)| !b.is_null())
                .map(|(a, b)| format!("{}: {}", title(a), show(b)))
                .collect::<Vec<_>>()
                .join(", "),
            Value::Array(a) => a.iter().map(show).collect::<Vec<_>>().join(", "),
            other => show(other),
        };
        rows.push(format!("| {} | {} |", title(k), cell));
    }
    rows.join("\n")
}

fn list_dir(rel: &str) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(path(rel))
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

fn photos_section(slug: &str) -> String {
    let mut out = Vec::new();
    for (kind, label) in [("exterior", "Exterior"), ("interior", "Interior")] {
        let files = list_dir(&format!("cars/{slug}/photos/{kind}"));
        if files.is_empty() {
            continue;
        }
        out.push(format!("### {label}\n"));
        for f in files {
            let angle = f.split('-').nth(1).unwrap_or("");
            out.push(format!(r#"<img src="photos/{kind}/{f}" alt="{slug} {kind} view {angle}" width="420">"#));
        }
        out.push(String::new());
    }
    out.join("\n")
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().map(show).collect()
}

fn render(slug: &str, s: &Value) -> String {
    let mut p: Vec<String> = Vec::new();
    let bullets = |p: &mut Vec<String>, v: &Value| p.extend(strs(v).into_iter().map(|x| format!("- {x}")));

    p.push(format!("# {} {} {}\n", show(&s["make"]), show(&s["model"]), s["model_year"]));
    p.push(format!("*{} | {} | {}*\n", show(&s["segment"]), show(&s["body_style"]), show(&s["generation"])));
    p.push(format!(
        "**Price range (new, MSRP):** {} - {}  ",
        money(&s["msrp_usd"]["base"]),
        money(&s["msrp_usd"]["top"])
    ));
    let u = &s["typical_used_price_usd"];
    if u.as_object().is_some_and(|o| !o.is_empty()) {
        p.push(format!(
            "**Typical used:** {} at 2-3 years, {} at 5 years  ",
            money(&u["2_3_years"]),
            money(&u["5_years"])
        ));
    }
    p.push(format!("**Built in:** {}\n", show(&s["country_of_origin"])));

    p.push("## Photos\n".into());
    p.push(photos_section(slug));
    p.push("Photo sources and licenses: [`photo-credits.json`](photo-credits.json)\n".into());

    p.push("## Why a buyer picks this car\n".into());
    bullets(&mut p, &s["strengths"]);
    p.push(String::new());
    p.push("## Where it falls short\n".into());
    bullets(&mut p, &s["weaknesses"]);
    p.push(String::new());

    p.push(format!("**Ideal buyer:** {}\n", show(&s["ideal_buyer"])));
    p.push(format!("**Good fit for:** {}\n", strs(&s["use_cases"]).join(", ")));

    p.push("## Powertrains\n".into());
    for pt in s["powertrains"].as_array().into_iter().flatten() {
        p.push(format!("### {}\n", show(&pt["name"])));
        p.push(kv_table(pt, &["name"]));
        p.push(String::new());
    }

    if s.get("charging").is_some() {
        p.push("## Charging\n".into());
        p.push(kv_table(&s["charging"], &[]));
        p.push(String::new());
    }

    p.push("## Dimensions\n".into());
    p.push(kv_table(&s["dimensions"], &[]));
    p.push("\n## Capacity\n".into());
    p.push(kv_table(&s["capacity"], &[]));

    p.push("\n## Safety\n".into());
    let sf = &s["safety"];
    let stars = match &sf["nhtsa_overall_stars"] {
        Value::Null => "not rated".to_string(),
        Value::Number(n) if n.as_f64() == Some(0.0) => "not rated".to_string(),
        v => show(v),
    };
    p.push(format!("- **NHTSA overall:** {stars}"));
    p.push(format!("- **IIHS:** {}", show(&sf["iihs"])));
    p.push("- **Driver assistance:**".into());
    p.extend(strs(&sf["standard_adas"]).into_iter().map(|x| format!("  - {x}")));

    p.push("\n## Warranty\n".into());
    p.push(kv_table(&s["warranty"], &[]));

    p.push("\n## Technology and comfort\n".into());
    p.push(kv_table(&s["tech"], &["key_features"]));
    p.push("\n**Notable features:**\n".into());
    bullets(&mut p, &s["tech"]["key_features"]);

    p.push("\n## Trims\n".into());
    let trims = strs(&s["trims"]).join(", ");
    p.push(if trims.is_empty() { "n/a".into() } else { trims });
    p.push("\n## Colors and materials\n".into());
    p.push(format!("**Exterior:** {}", strs(&s["colors_exterior"]).join(", ")));
    p.push(format!("\n**Interior:** {}", strs(&s["interior_materials"]).join(", ")));

    p.push("\n## Ownership costs\n".into());
    p.push(kv_table(&s["ownership"], &[]));

    p.push("\n## Cross-shopped against\n".into());
    p.push(strs(&s["competitors"]).join(", "));

    p.push("\n## Handling objections\n".into());
    for o in s["sales_objections"].as_array().into_iter().flatten() {
        p.push(format!("**\"{}\"**\n", show(&o["objection"])));
        p.push(format!("> {}\n", show(&o["response"])));
    }

    p.push("## Notes for the salesperson\n".into());
    bullets(&mut p, &s["talk_tracks"]);

    let f = &s["financing_example"];
    if f.as_object().is_some_and(|o| !o.is_empty()) {
        p.push("\n## Sample payment\n".into());
        p.push(kv_table(f, &["note"]));
        p.push(format!("\n*{}*", show(&f["note"])));
    }

    p.push(format!("\n---\n\n*{}*\n", show(&s["data_disclaimer"])));
    p.join("\n")
}

fn main() -> Result<()> {
    let mut catalog = Vec::new();
    for spec_path in spec_paths()? {
        let slug = spec_path.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        let s: Value = serde_json::from_str(&fs::read_to_string(&spec_path)?)
            .with_context(|| format!("parsing {}", spec_path.display()))?;
        fs::write(path(&format!("cars/{slug}/README.md")), render(&slug, &s))?;

        let pt = &s["powertrains"][0];
        let combined = match &pt["epa_mpg"]["combined"] {
            Value::Null => pt.get("epa_range_mi").cloned().unwrap_or(Value::Null),
            v => v.clone(),
        };
        let mut photos = Map::new();
        for k in ["exterior", "interior"] {
            photos.insert(k.into(), json!(list_dir(&format!("cars/{slug}/photos/{k}"))));
        }
        catalog.push(json!({
            "id": s["id"],
            "make": s["make"],
            "model": s["model"],
            "model_year": s["model_year"],
            "segment": s["segment"],
            "body_style": s["body_style"],
            "msrp_usd": s["msrp_usd"],
            "seating": s["dimensions"]["seating"],
            "base_hp": pt["hp"],
            "drivetrain": pt["drivetrain"],
            "fuel": pt["fuel"],
            "epa_combined": combined,
            "folder": format!("cars/{slug}"),
            "photos": photos,
        }));
    }
    fs::write(path("catalog.json"), serde_json::to_string_pretty(&catalog)?)?;

    let mut rows = String::from("| Vehicle | Segment | Seats | Base MSRP | Powertrain | Folder |\n|---|---|---|---|---|---|");
    for c in &catalog {
        write!(
            rows,
            "\n| {} {} {} | {} | {} | {} | {} hp {} | [{}]({}/) |",
            show(&c["make"]), show(&c["model"]), c["model_year"], show(&c["segment"]), c["seating"],
            money(&c["msrp_usd"]["base"]), c["base_hp"], show(&c["drivetrain"]), show(&c["id"]), show(&c["folder"])
        )?;
    }
    let tmpl = fs::read_to_string(path("README.template.md"))?;
    fs::write(path("README.md"), tmpl.replace("{{CATALOG_TABLE}}", &rows))?;
    println!("built {} car pages, catalog.json and README.md", catalog.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_money() {
        assert_eq!(title("epa_mpg"), "Epa mpg");
        assert_eq!(title("zero_to_60_s"), "Zero to 60 s");
        assert_eq!(money(&json!(45400)), "$45,400");
        assert_eq!(money(&json!("n/a")), "n/a");
    }
}
