//! WHAT: downloads freely licensed car photos from Wikimedia Commons into each
//!       cars/<slug>/photos/{exterior,interior}/ folder.
//! WHY:  the demo needs real pictures, and only Commons gives them with a clear
//!       license and author for every file.
//! HOW:  for each car in the car list, search Commons with each query until the
//!       folder has 4 exterior / 3 interior photos. Titles must match the car's
//!       regex filters so a "Civic" search does not bring back a Camry. Every
//!       saved file is appended to photo-credits.json with source, author and
//!       license.
//!
//!   cargo run -p datagen --bin fetch_photos -- scripts/cars.json scripts/filters.json

use anyhow::{Context, Result};
use datagen::path;
use regex::Regex;
use serde_json::{json, Value};
use std::{fs, thread, time::Duration};

const API: &str = "https://commons.wikimedia.org/w/api.php";
const UA: &str = "sales-project-demo/1.0 (demo dataset builder)";

struct Image {
    title: String,
    src: String,
    page: String,
    license: String,
    artist: String,
}

fn api(http: &reqwest::blocking::Client, query: &str) -> Value {
    let params = [
        ("action", "query"), ("generator", "search"), ("gsrsearch", &format!("filetype:bitmap {query}")),
        ("gsrnamespace", "6"), ("gsrlimit", "12"), ("prop", "imageinfo"),
        ("iiprop", "url|extmetadata|size"), ("iiurlwidth", "1600"), ("format", "json"), ("formatversion", "2"),
    ];
    for attempt in 0..3 {
        let r = http.get(API).query(&params).send().and_then(|r| r.error_for_status()).and_then(|r| r.json());
        match r {
            Ok(v) => return v,
            Err(e) if attempt == 2 => println!("  api fail: {e}"),
            Err(_) => thread::sleep(Duration::from_secs(6)),
        }
    }
    Value::Null
}

fn search_images(http: &reqwest::blocking::Client, tags: &Regex, query: &str) -> Vec<Image> {
    thread::sleep(Duration::from_millis(1500)); // be polite to the Commons API
    let d = api(http, query);
    let text = |v: &Value| v.as_str().unwrap_or("unknown").to_string();
    let mut out = Vec::new();
    for p in d["query"]["pages"].as_array().into_iter().flatten() {
        let ii = &p["imageinfo"][0];
        let Some(url) = ii["url"].as_str() else { continue };
        if ii["width"].as_u64().unwrap_or(0) < 800 {
            continue;
        }
        let meta = &ii["extmetadata"];
        out.push(Image {
            title: text(&p["title"]),
            src: ii["thumburl"].as_str().unwrap_or(url).to_string(),
            page: ii["descriptionurl"].as_str().unwrap_or("").to_string(),
            license: text(&meta["LicenseShortName"]["value"]),
            artist: tags.replace_all(&text(&meta["Artist"]["value"]), "").trim().to_string(),
        });
    }
    out
}

fn slugify(s: &str) -> String {
    let re = Regex::new("[^a-z0-9]+").unwrap();
    re.replace_all(&s.to_lowercase(), "-").trim_matches('-').to_string()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, cars_file, filters_file] = args.as_slice() else {
        anyhow::bail!("usage: fetch_photos <cars.json> <filters.json>");
    };
    let cars: Vec<Value> = serde_json::from_str(&fs::read_to_string(cars_file)?).context("cars.json")?;
    let filters: Value = serde_json::from_str(&fs::read_to_string(filters_file)?).context("filters.json")?;
    let interior_re = Regex::new("interior|dashboard|cockpit|cabin|innenraum|seat|steering|instrument")?;
    let tags = Regex::new("<[^>]+>")?;
    let http = reqwest::blocking::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(60))
        .build()?;

    for car in &cars {
        let slug = car["slug"].as_str().context("car without slug")?;
        let car_filters: Vec<Regex> = filters[slug]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().and_then(|p| Regex::new(p).ok()))
            .collect();
        let folder = path(&format!("cars/{slug}"));
        let mut credits = Vec::new();

        for (kind, key, want) in [("exterior", "ext_queries", 4), ("interior", "int_queries", 3)] {
            let dir = folder.join("photos").join(kind);
            fs::create_dir_all(&dir)?;
            let mut have = fs::read_dir(&dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".jpg") || e.file_name().to_string_lossy().ends_with(".png"))
                .count();
            let mut seen = std::collections::HashSet::new();
            for q in car[key].as_array().into_iter().flatten().filter_map(|q| q.as_str()) {
                if have >= want {
                    break;
                }
                for img in search_images(&http, &tags, q) {
                    if have >= want {
                        break;
                    }
                    let t = img.title.to_lowercase();
                    if seen.contains(&img.title) || !car_filters.iter().all(|re| re.is_match(&t)) {
                        continue;
                    }
                    if kind == "interior" && !interior_re.is_match(&t) {
                        continue;
                    }
                    seen.insert(img.title.clone());
                    let ext = if img.src.to_lowercase().ends_with(".png") { ".png" } else { ".jpg" };
                    let stem: String = slugify(&img.title.replace("File:", "")).chars().take(60).collect();
                    let name = format!("{kind}-{:02}-{stem}{ext}", have + 1);
                    let bytes = match http.get(&img.src).send().and_then(|r| r.error_for_status()).and_then(|r| r.bytes()) {
                        Ok(b) => b,
                        Err(e) => {
                            println!("  dl fail: {e}");
                            continue;
                        }
                    };
                    if bytes.len() < 5000 {
                        continue; // an error page, not a photo
                    }
                    fs::write(dir.join(&name), &bytes)?;
                    have += 1;
                    credits.push(json!({
                        "file": format!("photos/{kind}/{name}"),
                        "source": img.page,
                        "license": img.license,
                        "author": img.artist,
                    }));
                    println!("  {slug}/{name}");
                }
            }
        }

        let cf = folder.join("photo-credits.json");
        let mut all: Vec<Value> = match fs::read_to_string(&cf) {
            Ok(s) => serde_json::from_str(&s)?,
            Err(_) => Vec::new(),
        };
        let added = credits.len();
        all.extend(credits);
        fs::write(&cf, serde_json::to_string_pretty(&all)?)?;
        println!("{slug} done: {added} new photos");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn slugify_matches_file_names() {
        assert_eq!(super::slugify("2020 Subaru Outback (front).jpg"), "2020-subaru-outback-front-jpg");
    }
}
