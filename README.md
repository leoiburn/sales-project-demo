# sales-project-demo

Demo vehicle database for a **car dealership sales agent**. Twenty well-known vehicles, each in its own
folder with photos, full specifications, buying strengths and weaknesses, ownership costs and
objection-handling notes.

Built to be read by an AI sales agent as well as by a person: every car has a structured
`specs.json` (machine-readable) and a generated `README.md` (human-readable) with the same content.

## Catalog

| Vehicle | Segment | Seats | Base MSRP | Powertrain | Folder |
|---|---|---|---|---|---|
| Audi Q5 2025 | Compact luxury SUV | 5 | $45,400 | 261 hp quattro AWD | [audi-q5](cars/audi-q5/) |
| BMW 3 Series 2025 | Compact executive / luxury sport sedan | 5 | $46,000 | 255 hp RWD or xDrive AWD | [bmw-3-series](cars/bmw-3-series/) |
| Chevrolet Corvette (C8) 2025 | Sports car / supercar | 2 | $69,000 | 495 hp RWD | [chevrolet-corvette-c8](cars/chevrolet-corvette-c8/) |
| Chevrolet Silverado 1500 2025 | Full-size light-duty pickup | 6 | $37,700 | 310 hp RWD or 4WD | [chevrolet-silverado-1500](cars/chevrolet-silverado-1500/) |
| Ford F-150 2025 | Full-size light-duty pickup | 6 | $38,810 | 325 hp RWD or 4WD | [ford-f-150](cars/ford-f-150/) |
| Honda Civic 2025 | Compact car | 5 | $24,250 | 150 hp FWD | [honda-civic](cars/honda-civic/) |
| Hyundai Tucson 2025 | Compact SUV | 5 | $28,500 | 187 hp FWD or HTRAC AWD | [hyundai-tucson](cars/hyundai-tucson/) |
| Jeep Wrangler 2025 | Compact off-road SUV | 5 | $33,990 | 270 hp 4WD | [jeep-wrangler](cars/jeep-wrangler/) |
| Kia Telluride 2025 | Midsize 3-row SUV | 8 | $37,000 | 291 hp FWD or AWD with locking center differential | [kia-telluride](cars/kia-telluride/) |
| Lexus RX 2025 | Midsize luxury SUV | 5 | $50,000 | 275 hp FWD or AWD | [lexus-rx](cars/lexus-rx/) |
| Mazda CX-5 2025 | Compact SUV | 5 | $29,300 | 187 hp i-ACTIV AWD standard | [mazda-cx-5](cars/mazda-cx-5/) |
| Mercedes-Benz C-Class 2025 | Compact executive / luxury sedan | 5 | $48,500 | 255 hp RWD or 4MATIC AWD | [mercedes-benz-c-class](cars/mercedes-benz-c-class/) |
| Nissan Altima 2025 | Midsize sedan | 5 | $26,500 | 188 hp FWD | [nissan-altima](cars/nissan-altima/) |
| Porsche 911 2025 | Luxury sports car | 4 | $120,100 | 388 hp RWD | [porsche-911](cars/porsche-911/) |
| Ram 1500 2025 | Full-size light-duty pickup | 6 | $40,000 | 305 hp RWD or 4WD | [ram-1500](cars/ram-1500/) |
| Subaru Outback 2025 | Midsize crossover wagon | 5 | $30,000 | 182 hp Symmetrical AWD | [subaru-outback](cars/subaru-outback/) |
| Tesla Model 3 2025 | Compact / midsize electric luxury sedan | 5 | $42,490 | 283 hp RWD | [tesla-model-3](cars/tesla-model-3/) |
| Toyota Camry 2025 | Midsize sedan | 5 | $28,700 | 225 hp FWD | [toyota-camry](cars/toyota-camry/) |
| Toyota RAV4 2025 | Compact SUV | 5 | $29,000 | 203 hp FWD or AWD | [toyota-rav4](cars/toyota-rav4/) |
| Volkswagen Golf GTI 2025 | Hot hatch / sport compact | 5 | $32,500 | 241 hp FWD with VAQ electronic limited-slip differential | [volkswagen-golf-gti](cars/volkswagen-golf-gti/) |

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
