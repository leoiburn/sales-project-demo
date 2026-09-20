#!/usr/bin/env python3
"""Self-check: every car folder is complete and catalog.json agrees with the specs.

    python3 scripts/test_catalog.py
"""
import json, os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CARS = os.path.join(ROOT, "cars")

REQUIRED = ["id", "make", "model", "model_year", "segment", "body_style", "msrp_usd",
            "powertrains", "dimensions", "capacity", "safety", "warranty", "tech",
            "strengths", "weaknesses", "ideal_buyer", "ownership", "competitors",
            "sales_objections", "data_disclaimer"]

slugs = sorted(d for d in os.listdir(CARS) if os.path.isdir(os.path.join(CARS, d)))
assert len(slugs) == 20, f"expected 20 cars, found {len(slugs)}"

catalog = json.load(open(os.path.join(ROOT, "catalog.json")))
assert [c["id"] for c in catalog] == slugs, "catalog.json is out of sync with cars/ - rerun build_docs.py"

for slug in slugs:
    folder = os.path.join(CARS, slug)
    spec = json.load(open(os.path.join(folder, "specs.json")))
    missing = [k for k in REQUIRED if k not in spec]
    assert not missing, f"{slug}: specs.json missing {missing}"
    assert spec["id"] == slug, f"{slug}: id field is {spec['id']}"
    assert spec["msrp_usd"]["base"] < spec["msrp_usd"]["top"], f"{slug}: msrp range inverted"
    assert spec["powertrains"], f"{slug}: no powertrains"
    assert os.path.exists(os.path.join(folder, "README.md")), f"{slug}: README.md not generated"

    for kind, minimum in (("exterior", 4), ("interior", 3)):
        photos = os.listdir(os.path.join(folder, "photos", kind))
        assert len(photos) >= minimum, f"{slug}: {len(photos)} {kind} photos, expected {minimum}"

    credits = json.load(open(os.path.join(folder, "photo-credits.json")))
    credited = {c["file"] for c in credits}
    for kind in ("exterior", "interior"):
        for f in os.listdir(os.path.join(folder, "photos", kind)):
            rel = f"photos/{kind}/{f}"
            assert rel in credited, f"{slug}: {rel} has no license credit"
    for c in credits:
        assert os.path.exists(os.path.join(folder, c["file"])), f"{slug}: credit points at missing {c['file']}"

print(f"ok - {len(slugs)} cars, {sum(len(c['photos']['exterior']) + len(c['photos']['interior']) for c in catalog)} photos, all credited")
