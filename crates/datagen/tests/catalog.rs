//! WHAT: every car folder is complete and catalog.json agrees with the specs.
//! WHY:  catch a missing photo credit or a stale catalog before it reaches the
//!       loader or a published README.
//!
//!   cargo test -p datagen --test catalog

use datagen::path;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;

const REQUIRED: &[&str] = &[
    "id", "make", "model", "model_year", "segment", "body_style", "msrp_usd", "powertrains",
    "dimensions", "capacity", "safety", "warranty", "tech", "strengths", "weaknesses",
    "ideal_buyer", "ownership", "competitors", "sales_objections", "data_disclaimer",
];

fn read_json(rel: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(path(rel)).unwrap()).unwrap()
}

fn names(rel: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(path(rel))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn catalog_is_complete_and_credited() {
    let slugs: Vec<String> = names("cars").into_iter().filter(|s| path(&format!("cars/{s}")).is_dir()).collect();
    assert_eq!(slugs.len(), 20, "expected 20 cars");

    let catalog = read_json("catalog.json");
    let ids: Vec<&str> = catalog.as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, slugs, "catalog.json is out of sync with cars/ - rerun build_docs");

    for slug in &slugs {
        let spec = read_json(&format!("cars/{slug}/specs.json"));
        let missing: Vec<_> = REQUIRED.iter().filter(|k| spec.get(**k).is_none()).collect();
        assert!(missing.is_empty(), "{slug}: specs.json missing {missing:?}");
        assert_eq!(spec["id"], slug.as_str(), "{slug}: wrong id field");
        assert!(spec["msrp_usd"]["base"].as_f64() < spec["msrp_usd"]["top"].as_f64(), "{slug}: msrp range inverted");
        assert!(!spec["powertrains"].as_array().unwrap().is_empty(), "{slug}: no powertrains");
        assert!(path(&format!("cars/{slug}/README.md")).exists(), "{slug}: README.md not generated");

        let credits = read_json(&format!("cars/{slug}/photo-credits.json"));
        let credited: HashSet<&str> = credits.as_array().unwrap().iter().map(|c| c["file"].as_str().unwrap()).collect();
        for (kind, minimum) in [("exterior", 4), ("interior", 3)] {
            let photos = names(&format!("cars/{slug}/photos/{kind}"));
            assert!(photos.len() >= minimum, "{slug}: {} {kind} photos, expected {minimum}", photos.len());
            for f in photos {
                let rel = format!("photos/{kind}/{f}");
                assert!(credited.contains(rel.as_str()), "{slug}: {rel} has no license credit");
            }
        }
        for file in &credited {
            assert!(path(&format!("cars/{slug}/{file}")).exists(), "{slug}: credit points at missing {file}");
        }
    }
}
