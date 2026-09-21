"""
WHAT: Builds a synthetic dealer inventory (seed/inventory.json) from the model
      catalog in cars/*/specs.json.
WHY:  The repo describes car MODELS ("what a 2025 Q5 is"). A dealership database
      needs UNITS ("this Q5, VIN ..., 31,402 miles, $38,995, on the lot since
      March"). Those unit-level facts do not exist anywhere in the repo, so for
      this fictional demo dealer they are generated here. Every field that is
      invented is derived from real catalog data (MSRP, used-price anchors,
      trims, colors, powertrains) so the numbers stay internally consistent.
HOW:  Fixed RNG seed -> the same inventory every run, which is what makes the
      loader idempotent. VINs carry a correct ISO 3779 check digit, so a VIN
      decoder treats them as well-formed.

This data is FICTIONAL. It describes no real vehicle and no real person.
"""
import json
import glob
import random
import re
from datetime import date, timedelta

SEED = 20260921
CURRENT_YEAR = 2025
TODAY = date(2026, 9, 21)

# WMI (first 3 VIN chars) per make. Real public manufacturer codes, so the VIN
# decodes to the right brand; the 14 chars after them are generated.
WMI = {
    "Audi": "WA1", "BMW": "WBA", "Chevrolet": "1G1", "Ford": "1FT",
    "Honda": "1HG", "Hyundai": "5NP", "Jeep": "1C4", "Kia": "5XY",
    "Lexus": "2T2", "Mazda": "JM3", "Mercedes-Benz": "W1K", "Nissan": "1N4",
    "Porsche": "WP0", "Ram": "1C6", "Subaru": "4S4", "Tesla": "5YJ",
    "Toyota": "4T1", "Volkswagen": "3VW",
}
# VIN position 10 = model year.
YEAR_CODE = {2018: "J", 2019: "K", 2020: "L", 2021: "M", 2022: "N",
             2023: "P", 2024: "R", 2025: "S", 2026: "T"}
VIN_CHARS = "0123456789ABCDEFGHJKLMNPRSTUVWXYZ"  # no I, O, Q
TRANSLIT = {**{str(d): d for d in range(10)},
            **{c: v for c, v in zip("ABCDEFGH", range(1, 9))},
            **{c: v for c, v in zip("JKLMN", range(1, 6))},
            "P": 7, "R": 9,
            **{c: v for c, v in zip("STUVWXYZ", range(2, 10))}}
WEIGHTS = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2]

BODY_RULES = [  # first match wins, checked against segment + body_style
    ("truck", ["pickup"]),
    ("wagon", ["wagon"]),
    ("hatchback", ["hot hatch", "hatch"]),
    ("convertible", ["convertible", "roadster"]),
    ("coupe", ["sports car", "supercar", "2-door"]),
    ("suv", ["suv", "crossover"]),
    ("van", ["minivan", "van"]),
    ("sedan", ["sedan", "compact car"]),
]


def vin_check_digit(vin17):
    """ISO 3779: weighted sum of transliterated chars, mod 11, 10 -> 'X'."""
    total = sum(TRANSLIT[c] * w for c, w in zip(vin17, WEIGHTS))
    r = total % 11
    return "X" if r == 10 else str(r)


def make_vin(rng, make, year):
    wmi = WMI[make]
    vds = "".join(rng.choice(VIN_CHARS) for _ in range(5))      # pos 4-8
    plant = rng.choice("ABCDEFGHJKLMNPRSTUVWXYZ")               # pos 11
    serial = f"{rng.randrange(100000, 999999)}"                 # pos 12-17
    body = f"{wmi}{vds}0{YEAR_CODE[year]}{plant}{serial}"       # 0 = placeholder
    return body[:8] + vin_check_digit(body) + body[9:]


def body_type_of(spec):
    hay = (spec["segment"] + " " + spec["body_style"]).lower()
    for bt, words in BODY_RULES:
        if any(w in hay for w in words):
            return bt
    raise ValueError(f"no body_type for {spec['id']}: {hay[:80]}")


def doors_of(spec, body_type):
    m = re.search(r"(\d)-door", spec["body_style"].lower())
    if m:
        return int(m.group(1))
    return {"coupe": 2, "convertible": 2}.get(body_type, 4)


def price_cents(spec, year, mileage, condition, rng):
    """Interpolate MSRP -> 2-3yr -> 5yr anchors by age, then adjust."""
    base = spec["msrp_usd"]["base"]
    used = spec["typical_used_price_usd"]
    age = CURRENT_YEAR - year
    if condition == "new":
        # new units sit at or slightly over MSRP (market adjustment)
        usd = base * rng.uniform(1.00, 1.06)
    elif age <= 2.5:
        t = age / 2.5
        usd = base + (used["2_3_years"] - base) * t
    elif age <= 5:
        t = (age - 2.5) / 2.5
        usd = used["2_3_years"] + (used["5_years"] - used["2_3_years"]) * t
    else:
        # past the 5-year anchor, ~8%/yr further decline
        usd = used["5_years"] * (0.92 ** (age - 5))
    if condition != "new":
        expected = 12000 * max(age, 0.5)
        usd *= 1.0 - max(-0.08, min(0.10, (mileage - expected) / expected * 0.12))
        usd *= rng.uniform(0.96, 1.04)
        if condition == "cpo":
            usd *= 1.04          # CPO carries a premium for the warranty
    cents = int(round(usd / 50.0) * 50 * 100)   # round to nearest $50
    assert cents > 0
    return cents


def pick_drivetrain(raw, rng):
    """'RWD or 4WD' is a model option list. A unit has exactly one."""
    if not raw:
        return None
    opts = [o.strip() for o in re.split(r"\bor\b|/", raw) if o.strip()]
    return rng.choice(opts) if len(opts) > 1 else raw.strip()


def interior_color(spec, rng):
    raw = rng.choice(spec["interior_materials"])
    return re.sub(r"\s*\(.*?\)", "", raw).strip()


def build():
    rng = random.Random(SEED)
    units, internals, photos = [], [], []
    stock_n = 0

    for path in sorted(glob.glob("cars/*/specs.json")):
        spec = json.load(open(path))
        folder = path.rsplit("/", 1)[0]
        catalog_photos = sorted(glob.glob(f"{folder}/photos/exterior/*")) + \
            sorted(glob.glob(f"{folder}/photos/interior/*"))
        bt = body_type_of(spec)
        pts = spec["powertrains"]

        for _ in range(rng.randint(2, 5)):
            stock_n += 1
            # age profile: most of the lot is 1-4 years old
            year = CURRENT_YEAR - rng.choices([0, 1, 2, 3, 4, 5, 6],
                                              weights=[18, 20, 20, 15, 12, 9, 6])[0]
            age = CURRENT_YEAR - year
            if age == 0 and rng.random() < 0.55:
                condition, mileage = "new", rng.randint(4, 60)
            else:
                mileage = max(500, int(rng.gauss(12000 * max(age, 0.6), 3500)))
                # CPO per the knowledge base: <= 6 model years, under 85k miles
                condition = "cpo" if (age <= 6 and mileage < 85000
                                      and rng.random() < 0.35) else "used"
            status = rng.choices(["available", "pending", "sold"],
                                 weights=[80, 12, 8])[0]
            pt = rng.choice(pts)
            mpg = pt.get("epa_mpg") or {}
            features = list(dict.fromkeys(
                spec["safety"].get("standard_adas", [])[:4] +
                [v for v in spec["tech"].values() if isinstance(v, str)][:3]))
            vid = f"{spec['id']}-{stock_n:04d}"

            units.append({
                "unit_id": vid,
                "catalog_id": spec["id"],
                "stock_number": f"AX{str(year)[2:]}-{stock_n:04d}",
                "vin": make_vin(rng, spec["make"], year),
                "condition": condition,
                "status": status,
                "year": year,
                "make": spec["make"],
                "model": spec["model"],
                "trim_level": rng.choice(spec["trims"]),
                "body_type": bt,
                "doors": doors_of(spec, bt),
                "exterior_color": rng.choice(spec["colors_exterior"]),
                "interior_color": interior_color(spec, rng),
                "engine": pt.get("engine") or pt.get("name"),
                "transmission": pt.get("transmission"),
                "drivetrain": pick_drivetrain(pt.get("drivetrain"), rng),
                "fuel_type": pt.get("fuel"),
                "mpg_city": mpg.get("city"),
                "mpg_hwy": mpg.get("highway") or mpg.get("hwy"),
                "mileage": f"{mileage:,}",
                "list_price": f"${price_cents(spec, year, mileage, condition, rng) // 100:,}",
                "msrp": f"${spec['msrp_usd']['base']:,}",
                "features": features,
                "description": " ".join(spec["strengths"][:2]),
                "date_in_stock": str(TODAY - timedelta(days=rng.randint(1, 180))),
            })

            lp = int(units[-1]["list_price"].replace("$", "").replace(",", ""))
            internals.append({
                "unit_id": vid,
                # dealers buy below retail: trade-in or auction, then recondition
                "acquisition_cost": f"${int(lp * rng.uniform(0.78, 0.88)):,}",
                "recon_cost": f"${rng.randrange(20, 320) * 5:,}",
                "acquired_from": rng.choice(
                    ["Trade-in", "Manheim San Antonio", "ADESA Dallas",
                     "Lease return", "Street purchase", "Franchise partner"]),
                "internal_notes": rng.choice(
                    ["Clean Carfax, 1 owner.", "Minor curb rash on front right wheel.",
                     "Needs front pads before delivery.", "Second key on order.",
                     "Detail complete, front-line ready.", "Priced to move, aged unit."]),
            })

            for pos, p in enumerate(catalog_photos, start=1):
                photos.append({"unit_id": vid, "position": pos,
                               "storage_path": p, "is_primary": pos == 1})

    return units, internals, photos


if __name__ == "__main__":
    units, internals, photos = build()
    json.dump({"dealer": {"name": "Automotrix",
                          "phone": "(210) 555-0142",
                          "address": "8420 Bandera Rd, San Antonio, TX 78250",
                          "timezone": "America/Chicago"},
               "vehicles": units, "internal": internals, "photos": photos},
              open("seed/inventory.json", "w"), indent=1)

    vins = [u["vin"] for u in units]
    assert len(set(vins)) == len(vins), "VIN duplicado"
    assert all(len(v) == 17 for v in vins)
    assert all(v[8] == vin_check_digit(v) for v in vins), "check digit malo"
    print(f"unidades: {len(units)}  fotos: {len(photos)}")
    print("VINs unicos y con check digit valido: OK")
    from collections import Counter
    print("condicion:", dict(Counter(u["condition"] for u in units)))
    print("status:   ", dict(Counter(u["status"] for u in units)))
    print("años:     ", dict(sorted(Counter(u["year"] for u in units).items())))
    print("ejemplo precio/millaje:", units[0]["list_price"], "/", units[0]["mileage"], "mi")
