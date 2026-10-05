# sales-project-demo 🚗

An **AI car salesperson** and the car data it learns from.

## What is it?
Imagine a chatbot on a car dealership website. A customer asks "Which SUV is good for a big family?" and the bot answers like a helpful salesperson, then books a test drive.

This project has two parts:
1. **The car library**: 20 popular cars. Each car has its own folder with photos, specs (size, power, price), good points, bad points, and answers to common customer worries.
2. **The bot (Automotrix)**: written in **Rust**. It searches the car library and the dealership's info, talks to an AI model, and saves customer contacts (leads) and appointments in a database (**Supabase / Postgres**).

## Safety rules
The bot has **guardrails**: rules saved right in the database that stop it from making promises it shouldn't, like exact prices or loan approvals.

Every car has a `specs.json` file (for the computer) and a `README.md` (for people), with the same info.

## Catalog

{{CATALOG_TABLE}}

## Layout

```
sales-project-demo/
├── README.md                # this file (generated)
├── README.template.md       # template the generator fills in
├── catalog.json             # flat index of all 20 vehicles, for loading into a database
├── crates/datagen/src/bin/
│   ├── build_docs.rs        # regenerates every README.md + catalog.json from the specs
│   └── fetch_photos.rs      # downloads freely licensed photos from Wikimedia Commons
├── scripts/                 # car list + title filters used by fetch_photos
└── cars/
    └── <make-model>/
        ├── specs.json           # source of truth
        ├── README.md            # generated from specs.json
        ├── photo-credits.json   # source URL, author and license for each photo
        └── photos/
            ├── exterior/        # 4 angles
            └── interior/        # 3 views
```

## What is in each `specs.json`

| Section | Contents |
|---|---|
| Identity | make, model, model year, generation, body style, segment, build country |
| Pricing | MSRP range, typical used values at 2-3 and 5 years, trim list |
| `powertrains[]` | engine, displacement, hp, torque, transmission, drivetrain, fuel, EPA mpg/range, 0-60 |
| `charging` | EV only: DC peak kW, 10-80% time, connector, network |
| `dimensions` / `capacity` | length, width, height, wheelbase, curb weight, clearance, cargo, towing, payload, tank |
| `safety` | NHTSA stars, IIHS result, standard driver-assistance systems |
| `warranty` | basic, powertrain, hybrid/EV battery, roadside, included maintenance |
| `tech` | screen sizes, audio, connectivity, notable features |
| `strengths` / `weaknesses` | honest selling points and honest drawbacks |
| `ideal_buyer` / `use_cases` | who the car is for |
| `ownership` | service interval, typical insurance, 5-year cost note, reliability note |
| `competitors` | what the customer is also looking at |
| `sales_objections[]` | the objection, and a straight answer to it |
| `talk_tracks` | how to run the test drive and the walkaround |
| `financing_example` | illustrative payment on a representative build |

## Regenerating

`specs.json` is the only file you edit by hand. After any change:

```bash
cargo run -p datagen --bin build_docs
```

That rewrites every `cars/*/README.md`, `catalog.json` and this `README.md`. `cargo test -p datagen --test catalog`
checks that every folder is complete and every photo is credited.

To add more photos for a car, add an entry to a car list and run:

```bash
cargo run -p datagen --bin fetch_photos -- scripts/cars.json scripts/filters.json
```

## Loading it into a database

`catalog.json` is a flat array, one object per vehicle, suitable for a direct import. For the full
record, read each `cars/<id>/specs.json` — the `id` field matches the folder name and the `folder`
field in the catalog.

```rust
let catalog: serde_json::Value = serde_json::from_str(&std::fs::read_to_string("catalog.json")?)?;
for c in catalog.as_array().unwrap() {
    let spec = std::fs::read_to_string(format!("{}/specs.json", c["folder"].as_str().unwrap()))?;
}
```

## Photos and licensing

All photos come from [Wikimedia Commons](https://commons.wikimedia.org) and are under free licenses
(Creative Commons or public domain). Each car folder has a `photo-credits.json` listing, per file,
the source page, the author and the license. **If you publish these images, reproduce the attribution
and license terms from that file** — most CC-BY-SA licenses require credit and share-alike.

The specification text and sales notes in this repository were written for this demo.

## Accuracy

Specifications, prices and EPA figures are approximate US-market values assembled for a demo. They
are close enough to exercise a sales agent, but they are **not** a substitute for the manufacturer's
window sticker. Verify anything before quoting a real customer.
