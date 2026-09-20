#!/usr/bin/env python3
"""Render cars/<slug>/README.md and the root catalog.json + README.md from each specs.json.

specs.json is the source of truth. Run this after editing any spec:
    python3 scripts/build_docs.py
"""
import json, os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CARS = os.path.join(ROOT, "cars")


def money(n):
    return f"${n:,.0f}" if isinstance(n, (int, float)) else str(n)


def title(k):
    return k.replace("_", " ").replace("usd", "USD").replace("mpg", "MPG").replace("cu ft", "cu ft").capitalize()


def kv_table(d, skip=()):
    rows = ["| Field | Value |", "|---|---|"]
    for k, v in d.items():
        if k in skip or v is None:
            continue
        if isinstance(v, dict):
            v = ", ".join(f"{title(a)}: {b}" for a, b in v.items() if b is not None)
        elif isinstance(v, list):
            v = ", ".join(str(x) for x in v)
        rows.append(f"| {title(k)} | {v} |")
    return "\n".join(rows)


def photos_section(slug):
    out = []
    for kind, label in (("exterior", "Exterior"), ("interior", "Interior")):
        d = os.path.join(CARS, slug, "photos", kind)
        files = sorted(os.listdir(d)) if os.path.isdir(d) else []
        if not files:
            continue
        out.append(f"### {label}\n")
        for f in files:
            angle = f.split("-")[1]
            out.append(f'<img src="photos/{kind}/{f}" alt="{slug} {kind} view {angle}" width="420">')
        out.append("")
    return "\n".join(out)


def render(slug, s):
    p = []
    p.append(f"# {s['make']} {s['model']} {s['model_year']}\n")
    p.append(f"*{s['segment']} | {s['body_style']} | {s['generation']}*\n")
    p.append(f"**Price range (new, MSRP):** {money(s['msrp_usd']['base'])} - {money(s['msrp_usd']['top'])}  ")
    u = s.get("typical_used_price_usd", {})
    if u:
        p.append(f"**Typical used:** {money(u.get('2_3_years'))} at 2-3 years, {money(u.get('5_years'))} at 5 years  ")
    p.append(f"**Built in:** {s['country_of_origin']}\n")

    p.append("## Photos\n")
    p.append(photos_section(slug))
    p.append("Photo sources and licenses: [`photo-credits.json`](photo-credits.json)\n")

    p.append("## Why a buyer picks this car\n")
    for x in s["strengths"]:
        p.append(f"- {x}")
    p.append("")

    p.append("## Where it falls short\n")
    for x in s["weaknesses"]:
        p.append(f"- {x}")
    p.append("")

    p.append(f"**Ideal buyer:** {s['ideal_buyer']}\n")
    p.append("**Good fit for:** " + ", ".join(s["use_cases"]) + "\n")

    p.append("## Powertrains\n")
    for pt in s["powertrains"]:
        p.append(f"### {pt['name']}\n")
        p.append(kv_table(pt, skip=("name",)))
        p.append("")

    if "charging" in s:
        p.append("## Charging\n")
        p.append(kv_table(s["charging"]))
        p.append("")

    p.append("## Dimensions\n")
    p.append(kv_table(s["dimensions"]))
    p.append("\n## Capacity\n")
    p.append(kv_table(s["capacity"]))

    p.append("\n## Safety\n")
    sf = s["safety"]
    p.append(f"- **NHTSA overall:** {sf['nhtsa_overall_stars'] or 'not rated'}")
    p.append(f"- **IIHS:** {sf['iihs']}")
    p.append("- **Driver assistance:**")
    for x in sf["standard_adas"]:
        p.append(f"  - {x}")

    p.append("\n## Warranty\n")
    p.append(kv_table(s["warranty"]))

    p.append("\n## Technology and comfort\n")
    t = s["tech"]
    p.append(kv_table({k: v for k, v in t.items() if k != "key_features"}))
    p.append("\n**Notable features:**\n")
    for x in t["key_features"]:
        p.append(f"- {x}")

    p.append("\n## Trims\n")
    p.append(", ".join(s.get("trims", [])) or "n/a")
    p.append("\n## Colors and materials\n")
    p.append("**Exterior:** " + ", ".join(s["colors_exterior"]))
    p.append("\n**Interior:** " + ", ".join(s["interior_materials"]))

    p.append("\n## Ownership costs\n")
    p.append(kv_table(s["ownership"]))

    p.append("\n## Cross-shopped against\n")
    p.append(", ".join(s["competitors"]))

    p.append("\n## Handling objections\n")
    for o in s["sales_objections"]:
        p.append(f'**"{o["objection"]}"**\n')
        p.append(f"> {o['response']}\n")

    p.append("## Notes for the salesperson\n")
    for x in s["talk_tracks"]:
        p.append(f"- {x}")

    f = s.get("financing_example")
    if f:
        p.append("\n## Sample payment\n")
        p.append(kv_table(f, skip=("note",)))
        p.append(f"\n*{f['note']}*")

    p.append(f"\n---\n\n*{s['data_disclaimer']}*\n")
    return "\n".join(p)


def main():
    catalog = []
    for slug in sorted(os.listdir(CARS)):
        spec_path = os.path.join(CARS, slug, "specs.json")
        if not os.path.exists(spec_path):
            continue
        s = json.load(open(spec_path))
        open(os.path.join(CARS, slug, "README.md"), "w").write(render(slug, s))
        pt = s["powertrains"][0]
        catalog.append({
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
            "epa_combined": pt.get("epa_mpg", {}).get("combined") or pt.get("epa_range_mi"),
            "folder": f"cars/{slug}",
            "photos": {
                k: sorted(os.listdir(os.path.join(CARS, slug, "photos", k)))
                for k in ("exterior", "interior")
            },
        })
    json.dump(catalog, open(os.path.join(ROOT, "catalog.json"), "w"), indent=2, ensure_ascii=False)

    rows = ["| Vehicle | Segment | Seats | Base MSRP | Powertrain | Folder |", "|---|---|---|---|---|---|"]
    for c in catalog:
        rows.append(
            f"| {c['make']} {c['model']} {c['model_year']} | {c['segment']} | {c['seating']} | "
            f"{money(c['msrp_usd']['base'])} | {c['base_hp']} hp {c['drivetrain']} | "
            f"[{c['id']}]({c['folder']}/) |"
        )
    tmpl = open(os.path.join(ROOT, "README.template.md")).read()
    open(os.path.join(ROOT, "README.md"), "w").write(tmpl.replace("{{CATALOG_TABLE}}", "\n".join(rows)))
    print(f"built {len(catalog)} car pages, catalog.json and README.md")


if __name__ == "__main__":
    main()
