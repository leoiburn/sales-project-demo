//! WHAT: builds a synthetic dealer inventory (seed/inventory.json) from the model
//!       catalog in cars/*/specs.json.
//! WHY:  the repo describes car MODELS ("what a 2025 Q5 is"). A dealership
//!       database needs UNITS ("this Q5, VIN ..., 31,402 miles, $38,995, on the
//!       lot since March"). Those unit-level facts do not exist anywhere in the
//!       repo, so for this fictional demo dealer they are generated here. Every
//!       invented field is derived from real catalog data (MSRP, used-price
//!       anchors, trims, colors, powertrains) so the numbers stay consistent.
//! HOW:  a ChaCha RNG with a fixed seed gives the same inventory on every run and
//!       every machine - that is what makes the loader idempotent. VINs carry a
//!       correct ISO 3779 check digit, so a VIN decoder treats them as well-formed.
//!       Money and mileage are written the way a DMS export writes them
//!       ("$28,800", "45,021"), so the loader's normalization is exercised.
//!
//! This data is FICTIONAL. It describes no real vehicle and no real person.
//!
//!   cargo run -p datagen --bin gen_inventory

use anyhow::{anyhow, bail, Result};
use chrono::{Duration, NaiveDate};
use datagen::{path, spec_paths, thousands};
use rand::distributions::WeightedIndex;
use rand::prelude::*;
use rand_chacha::ChaCha8Rng;
use rand_distr::Normal;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

const SEED: u64 = 20260921;
const CURRENT_YEAR: i32 = 2025;

/// First three VIN characters per make: real public manufacturer codes, so the
/// VIN decodes to the right brand. The other fourteen are generated.
fn wmi(make: &str) -> Option<&'static str> {
    Some(match make {
        "Audi" => "WA1", "BMW" => "WBA", "Chevrolet" => "1G1", "Ford" => "1FT",
        "Honda" => "1HG", "Hyundai" => "5NP", "Jeep" => "1C4", "Kia" => "5XY",
        "Lexus" => "2T2", "Mazda" => "JM3", "Mercedes-Benz" => "W1K", "Nissan" => "1N4",
        "Porsche" => "WP0", "Ram" => "1C6", "Subaru" => "4S4", "Tesla" => "5YJ",
        "Toyota" => "4T1", "Volkswagen" => "3VW",
        _ => return None,
    })
}

/// VIN position 10 = model year.
fn year_code(year: i32) -> Option<char> {
    Some(match year {
        2018 => 'J', 2019 => 'K', 2020 => 'L', 2021 => 'M', 2022 => 'N',
        2023 => 'P', 2024 => 'R', 2025 => 'S', 2026 => 'T',
        _ => return None,
    })
}

const VIN_CHARS: &[u8] = b"0123456789ABCDEFGHJKLMNPRSTUVWXYZ"; // no I, O, Q
const PLANT_CHARS: &[u8] = b"ABCDEFGHJKLMNPRSTUVWXYZ";
const WEIGHTS: [u32; 17] = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2];

fn transliterate(c: char) -> u32 {
    match c {
        '0'..='9' => c as u32 - '0' as u32,
        'A' | 'J' => 1, 'B' | 'K' | 'S' => 2, 'C' | 'L' | 'T' => 3,
        'D' | 'M' | 'U' => 4, 'E' | 'N' | 'V' => 5, 'F' | 'W' => 6,
        'G' | 'P' | 'X' => 7, 'H' | 'Y' => 8, 'R' | 'Z' => 9,
        _ => 0,
    }
}

/// ISO 3779: weighted sum of transliterated chars, mod 11, 10 -> 'X'.
fn vin_check_digit(vin17: &str) -> char {
    let total: u32 = vin17
        .chars()
        .zip(WEIGHTS)
        .map(|(c, w)| transliterate(c) * w)
        .sum();
    match total % 11 {
        10 => 'X',
        r => char::from_digit(r, 10).unwrap(),
    }
}

fn make_vin(rng: &mut ChaCha8Rng, make: &str, year: i32) -> Result<String> {
    let wmi = wmi(make).ok_or_else(|| anyhow!("no WMI for make {make}"))?;
    let vds: String = (0..5).map(|_| *VIN_CHARS.choose(rng).unwrap() as char).collect();
    let plant = *PLANT_CHARS.choose(rng).unwrap() as char;
    let serial = rng.gen_range(100000..999999);
    let yc = year_code(year).ok_or_else(|| anyhow!("no year code for {year}"))?;
    // '0' is a placeholder for the check digit, computed over the full string
    let body = format!("{wmi}{vds}0{yc}{plant}{serial}");
    let check = vin_check_digit(&body);
    Ok(format!("{}{}{}", &body[..8], check, &body[9..]))
}

/// first match wins, checked against segment + body_style
const BODY_RULES: &[(&str, &[&str])] = &[
    ("truck", &["pickup"]),
    ("wagon", &["wagon"]),
    ("hatchback", &["hot hatch", "hatch"]),
    ("convertible", &["convertible", "roadster"]),
    ("coupe", &["sports car", "supercar", "2-door"]),
    ("suv", &["suv", "crossover"]),
    ("van", &["minivan", "van"]),
    ("sedan", &["sedan", "compact car"]),
];

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or_default()
}

fn body_type_of(spec: &Value) -> Result<&'static str> {
    let hay = format!("{} {}", s(spec, "segment"), s(spec, "body_style")).to_lowercase();
    for (bt, words) in BODY_RULES {
        if words.iter().any(|w| hay.contains(w)) {
            return Ok(bt);
        }
    }
    bail!("no body_type for {}: {}", s(spec, "id"), hay)
}

fn doors_of(spec: &Value, body_type: &str) -> i64 {
    let re = Regex::new(r"(\d)-door").unwrap();
    if let Some(c) = re.captures(&s(spec, "body_style").to_lowercase()) {
        return c[1].parse().unwrap_or(4);
    }
    match body_type {
        "coupe" | "convertible" => 2,
        _ => 4,
    }
}

/// Interpolate MSRP -> 2-3yr -> 5yr anchors by age, then adjust for mileage and
/// condition. Returns cents, rounded to the nearest $50.
fn price_cents(spec: &Value, year: i32, mileage: i64, condition: &str, rng: &mut ChaCha8Rng) -> i64 {
    let f = |p: &str| spec.pointer(p).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let base = f("/msrp_usd/base");
    let used_23 = f("/typical_used_price_usd/2_3_years");
    let used_5 = f("/typical_used_price_usd/5_years");
    let age = (CURRENT_YEAR - year) as f64;

    let mut usd = if condition == "new" {
        // new units sit at or slightly over MSRP (market adjustment)
        base * rng.gen_range(1.00..1.06)
    } else if age <= 2.5 {
        base + (used_23 - base) * (age / 2.5)
    } else if age <= 5.0 {
        used_23 + (used_5 - used_23) * ((age - 2.5) / 2.5)
    } else {
        // past the 5-year anchor, ~8%/yr further decline
        used_5 * 0.92_f64.powf(age - 5.0)
    };
    if condition != "new" {
        let expected = 12000.0 * age.max(0.5);
        let delta = ((mileage as f64 - expected) / expected * 0.12).clamp(-0.08, 0.10);
        usd *= 1.0 - delta;
        usd *= rng.gen_range(0.96..1.04);
        if condition == "cpo" {
            usd *= 1.04; // CPO carries a premium for the warranty
        }
    }
    let cents = ((usd / 50.0).round() * 50.0 * 100.0) as i64;
    assert!(cents > 0, "non-positive price for {}", s(spec, "id"));
    cents
}

/// "Leather-trimmed (Sport Touring)" -> "Leather-trimmed"
fn interior_color(spec: &Value, rng: &mut ChaCha8Rng) -> Option<String> {
    let opts = spec.get("interior_materials")?.as_array()?;
    let raw = opts.choose(rng)?.as_str()?;
    let re = Regex::new(r"\s*\(.*?\)").unwrap();
    Some(re.replace_all(raw, "").trim().to_string())
}

/// "RWD or 4WD" is a model option list. A unit has exactly one.
fn pick_drivetrain(raw: Option<&str>, rng: &mut ChaCha8Rng) -> Option<String> {
    let raw = raw?;
    let re = Regex::new(r"\bor\b|/").unwrap();
    let opts: Vec<&str> = re.split(raw).map(str::trim).filter(|o| !o.is_empty()).collect();
    if opts.len() > 1 {
        opts.choose(rng).map(|o| o.to_string())
    } else {
        Some(raw.trim().to_string())
    }
}

fn pick_str(v: &Value, key: &str, rng: &mut ChaCha8Rng) -> Option<String> {
    v.get(key)?.as_array()?.choose(rng)?.as_str().map(String::from)
}

fn list_files(dir: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

fn main() -> Result<()> {
    let mut rng = ChaCha8Rng::seed_from_u64(SEED);
    let today = NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
    let age_pick = WeightedIndex::new([18, 20, 20, 15, 12, 9, 6])?;
    let status_pick = WeightedIndex::new([80, 12, 8])?;
    const STATUSES: [&str; 3] = ["available", "pending", "sold"];

    let (mut units, mut internals, mut photos) = (Vec::new(), Vec::new(), Vec::new());
    let mut stock_n = 0;

    for spec_path in spec_paths()? {
        let spec: Value = serde_json::from_str(&std::fs::read_to_string(&spec_path)?)?;
        let id = s(&spec, "id").to_string();
        let folder = format!("cars/{id}");
        let mut catalog_photos = Vec::new();
        for kind in ["exterior", "interior"] {
            for f in list_files(&path(&format!("{folder}/photos/{kind}"))) {
                catalog_photos.push(format!("{folder}/photos/{kind}/{f}"));
            }
        }
        let bt = body_type_of(&spec)?;
        let pts = spec["powertrains"].as_array().cloned().unwrap_or_default();

        for _ in 0..rng.gen_range(2..=5) {
            stock_n += 1;
            // age profile: most of the lot is 1-4 years old
            let year = CURRENT_YEAR - age_pick.sample(&mut rng) as i32;
            let age = CURRENT_YEAR - year;
            let (condition, mileage) = if age == 0 && rng.gen::<f64>() < 0.55 {
                ("new", rng.gen_range(4..=60) as i64)
            } else {
                let mean = 12000.0 * (age as f64).max(0.6);
                let m = (Normal::new(mean, 3500.0)?.sample(&mut rng) as i64).max(500);
                // CPO per the knowledge base: <= 6 model years, under 85k miles
                let cpo = age <= 6 && m < 85000 && rng.gen::<f64>() < 0.35;
                (if cpo { "cpo" } else { "used" }, m)
            };
            let status = STATUSES[status_pick.sample(&mut rng)];
            let pt = pts.choose(&mut rng).cloned().unwrap_or(json!({}));
            let mpg = pt.get("epa_mpg").cloned().unwrap_or(json!({}));

            let mut features: Vec<String> = Vec::new();
            let mut seen = HashSet::new();
            let adas = spec.pointer("/safety/standard_adas").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            let tech: Vec<String> = spec
                .get("tech")
                .and_then(|x| x.as_object())
                .map(|o| o.values().filter_map(|v| v.as_str().map(String::from)).take(3).collect())
                .unwrap_or_default();
            for f in adas.iter().take(4).filter_map(|v| v.as_str().map(String::from)).chain(tech) {
                if seen.insert(f.clone()) {
                    features.push(f);
                }
            }

            let unit_id = format!("{id}-{stock_n:04}");
            let vin = make_vin(&mut rng, s(&spec, "make"), year)?;
            let price = price_cents(&spec, year, mileage, condition, &mut rng);
            let base = spec.pointer("/msrp_usd/base").and_then(|x| x.as_i64()).unwrap_or(0);
            let description = spec
                .get("strengths")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().take(2).filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();

            units.push(json!({
                "unit_id": unit_id,
                "catalog_id": id,
                "stock_number": format!("AX{}-{stock_n:04}", &year.to_string()[2..]),
                "vin": vin,
                "condition": condition,
                "status": status,
                "year": year,
                "make": s(&spec, "make"),
                "model": s(&spec, "model"),
                "trim_level": pick_str(&spec, "trims", &mut rng),
                "body_type": bt,
                "doors": doors_of(&spec, bt),
                "exterior_color": pick_str(&spec, "colors_exterior", &mut rng),
                "interior_color": interior_color(&spec, &mut rng),
                "engine": pt.get("engine").or_else(|| pt.get("name")),
                "transmission": pt.get("transmission"),
                "drivetrain": pick_drivetrain(pt.get("drivetrain").and_then(|x| x.as_str()), &mut rng),
                "fuel_type": pt.get("fuel"),
                "mpg_city": mpg.get("city"),
                "mpg_hwy": mpg.get("highway").or_else(|| mpg.get("hwy")),
                "mileage": thousands(mileage),
                "list_price": format!("${}", thousands(price / 100)),
                "msrp": format!("${}", thousands(base)),
                "features": features,
                "description": description,
                "date_in_stock": (today - Duration::days(rng.gen_range(1..=180))).to_string(),
            }));

            // dealers buy below retail: trade-in or auction, then recondition
            let lp = price / 100;
            internals.push(json!({
                "unit_id": unit_id,
                "acquisition_cost": format!("${}", thousands((lp as f64 * rng.gen_range(0.78..0.88)) as i64)),
                "recon_cost": format!("${}", thousands(rng.gen_range(20..320) * 5)),
                "acquired_from": *["Trade-in", "Manheim San Antonio", "ADESA Dallas",
                                   "Lease return", "Street purchase", "Franchise partner"].choose(&mut rng).unwrap(),
                "internal_notes": *["Clean Carfax, 1 owner.", "Minor curb rash on front right wheel.",
                                    "Needs front pads before delivery.", "Second key on order.",
                                    "Detail complete, front-line ready.", "Priced to move, aged unit."]
                                    .choose(&mut rng).unwrap(),
            }));

            for (i, p) in catalog_photos.iter().enumerate() {
                photos.push(json!({
                    "unit_id": unit_id, "position": i + 1,
                    "storage_path": p, "is_primary": i == 0,
                }));
            }
        }
    }

    // the invariants the loader will enforce anyway - fail here, closer to the cause
    let vins: Vec<&str> = units.iter().map(|u| u["vin"].as_str().unwrap()).collect();
    if vins.iter().collect::<HashSet<_>>().len() != vins.len() {
        bail!("duplicate VIN generated");
    }
    for v in &vins {
        if v.len() != 17 || v.chars().nth(8) != Some(vin_check_digit(v)) {
            bail!("bad VIN generated: {v}");
        }
    }

    let out = json!({
        "dealer": {
            "name": "Automotrix",
            "phone": "(210) 555-0142",
            "address": "8420 Bandera Rd, San Antonio, TX 78250",
            "timezone": "America/Chicago",
        },
        "vehicles": units,
        "internal": internals,
        "photos": photos,
    });
    std::fs::write(path("seed/inventory.json"), serde_json::to_string_pretty(&out)? + "\n")?;

    let tally = |key: &str| {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for u in &units {
            *m.entry(u[key].to_string().trim_matches('"').to_string()).or_default() += 1;
        }
        m
    };
    println!("unidades: {}  fotos: {}", units.len(), photos.len());
    println!("VINs unicos y con check digit valido: OK");
    println!("condicion: {:?}", tally("condition"));
    println!("status:    {:?}", tally("status"));
    println!("años:      {:?}", tally("year"));
    println!("ejemplo precio/millaje: {} / {} mi", units[0]["list_price"], units[0]["mileage"]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_digit_matches_a_known_vin() {
        // 1M8GDM9AXKP042788 is the worked example from the NHTSA VIN spec
        assert_eq!(vin_check_digit("1M8GDM9AXKP042788"), 'X');
    }

    #[test]
    fn generated_vins_are_well_formed() {
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        for _ in 0..200 {
            let v = make_vin(&mut rng, "Toyota", 2021).unwrap();
            assert_eq!(v.len(), 17);
            assert!(v.starts_with("4T1"));
            assert_eq!(v.chars().nth(9), Some('M'), "year code for 2021");
            assert_eq!(v.chars().nth(8), Some(vin_check_digit(&v)));
            assert!(!v.contains(['I', 'O', 'Q']));
        }
    }

    #[test]
    fn a_unit_gets_exactly_one_drivetrain() {
        let mut rng = ChaCha8Rng::seed_from_u64(2);
        for _ in 0..50 {
            let d = pick_drivetrain(Some("RWD or 4WD"), &mut rng).unwrap();
            assert!(d == "RWD" || d == "4WD", "{d}");
        }
        assert_eq!(pick_drivetrain(Some("quattro AWD"), &mut rng).as_deref(), Some("quattro AWD"));
    }

    #[test]
    fn same_seed_same_inventory() {
        let a: Vec<String> = { let mut r = ChaCha8Rng::seed_from_u64(SEED); (0..5).map(|_| make_vin(&mut r, "Ford", 2022).unwrap()).collect() };
        let b: Vec<String> = { let mut r = ChaCha8Rng::seed_from_u64(SEED); (0..5).map(|_| make_vin(&mut r, "Ford", 2022).unwrap()).collect() };
        assert_eq!(a, b);
    }
}
