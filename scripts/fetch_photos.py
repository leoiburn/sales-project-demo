#!/usr/bin/env python3
"""Download freely-licensed car photos from Wikimedia Commons into each car folder."""
import json, os, re, sys, time, urllib.parse, urllib.request

ROOT = "/home/leoiburn/sales-project-demo/cars"
API = "https://commons.wikimedia.org/w/api.php"
UA = "sales-project-demo/1.0 (demo dataset builder)"

CARS = json.load(open(sys.argv[1]))
FILTERS = json.load(open(sys.argv[2]))
INT_RE = re.compile(r"interior|dashboard|cockpit|cabin|innenraum|seat|steering|instrument")

def api(params):
    params = dict(params, format="json", formatversion="2")
    url = API + "?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.load(r)
        except Exception as e:
            if attempt == 2:
                print("  api fail:", e)
                return {}
            time.sleep(6)

def search_images(query, limit=12):
    time.sleep(1.5)
    d = api({"action": "query", "generator": "search", "gsrsearch": f"filetype:bitmap {query}",
             "gsrnamespace": 6, "gsrlimit": limit, "prop": "imageinfo",
             "iiprop": "url|extmetadata|size", "iiurlwidth": 1600})
    pages = d.get("query", {}).get("pages", [])
    out = []
    for p in pages:
        ii = (p.get("imageinfo") or [{}])[0]
        if not ii.get("url"):
            continue
        if ii.get("width", 0) < 800:
            continue
        meta = ii.get("extmetadata", {})
        out.append({
            "title": p["title"],
            "src": ii.get("thumburl") or ii["url"],
            "page": ii.get("descriptionurl", ""),
            "license": meta.get("LicenseShortName", {}).get("value", "unknown"),
            "artist": re.sub("<[^>]+>", "", meta.get("Artist", {}).get("value", "unknown")).strip(),
        })
    return out

def download(url, dest):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        data = r.read()
    if len(data) < 5000:
        return False
    open(dest, "wb").write(data)
    return True

def slug(s):
    return re.sub(r"[^a-z0-9]+", "-", s.lower()).strip("-")

for car in CARS:
    folder = os.path.join(ROOT, car["slug"])
    credits = []
    for kind, queries, want in (("exterior", car["ext_queries"], 4), ("interior", car["int_queries"], 3)):
        d = os.path.join(folder, "photos", kind)
        os.makedirs(d, exist_ok=True)
        have = len([f for f in os.listdir(d) if f.endswith((".jpg", ".png"))])
        seen = set()
        for q in queries:
            if have >= want:
                break
            for img in search_images(q):
                if have >= want:
                    break
                if img["title"] in seen:
                    continue
                t = img["title"].lower()
                if not all(re.search(p, t) for p in FILTERS.get(car["slug"], [])):
                    continue
                if kind == "interior" and not INT_RE.search(t):
                    continue
                seen.add(img["title"])
                ext = ".png" if img["src"].lower().endswith(".png") else ".jpg"
                name = f"{kind}-{have+1:02d}-{slug(img['title'].replace('File:',''))[:60]}{ext}"
                path = os.path.join(d, name)
                try:
                    if download(img["src"], path):
                        have += 1
                        credits.append({"file": f"photos/{kind}/{name}", "source": img["page"],
                                        "license": img["license"], "author": img["artist"]})
                        print(f"  {car['slug']}/{name}")
                except Exception as e:
                    print("  dl fail:", e)
    os.makedirs(folder, exist_ok=True)
    cf = os.path.join(folder, "photo-credits.json")
    old = json.load(open(cf)) if os.path.exists(cf) else []
    json.dump(old + credits, open(cf, "w"), indent=2, ensure_ascii=False)
    print(car["slug"], "done:", len(credits), "new photos")
