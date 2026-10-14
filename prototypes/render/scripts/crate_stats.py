#!/usr/bin/env python3
"""Print crates.io health data for crates: newest/stable version, total and
90-day downloads, last update, license and MSRV of the newest stable version,
and (optionally) the requirements it puts on selected dependencies.

Usage: python3 -I crate_stats.py wgpu winit egui [--deps wgpu,winit]
"""
import json, sys, time, urllib.request

UA = "real-engine-dev-tools/0.1 (render-spike crate_stats.py; noreply@anthropic.com)"


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def main(argv):
    deps = None
    names = []
    i = 0
    while i < len(argv):
        if argv[i] == "--deps":
            deps = set(argv[i + 1].split(","))
            i += 2
            continue
        names.append(argv[i])
        i += 1
    for n, name in enumerate(names):
        if n:
            time.sleep(1)
        try:
            data = get(f"https://crates.io/api/v1/crates/{name}")
        except Exception as e:  # report and continue
            print(f"== {name}: ERROR {e}")
            continue
        c = data["crate"]
        stable = c.get("max_stable_version") or c.get("max_version")
        ver = next((v for v in data.get("versions", []) if v["num"] == stable), None)
        lic = ver.get("license") if ver else None
        msrv = ver.get("rust_version") if ver else None
        published = (ver.get("created_at") or "")[:10] if ver else ""
        print(
            f"== {name}: stable={stable} ({published}) newest={c.get('newest_version')} "
            f"downloads={c.get('downloads'):,} recent90d={c.get('recent_downloads'):,} "
            f"updated={c.get('updated_at','')[:10]} license={lic} msrv={msrv} repo={c.get('repository')}"
        )
        if deps and stable:
            time.sleep(1)
            d = get(f"https://crates.io/api/v1/crates/{name}/{stable}/dependencies")["dependencies"]
            for dep in sorted(d, key=lambda x: x["crate_id"]):
                if dep["crate_id"] in deps and dep["kind"] == "normal":
                    opt = " (optional)" if dep.get("optional") else ""
                    tgt = f" target={dep['target']}" if dep.get("target") else ""
                    print(f"     needs {dep['crate_id']} {dep['req']}{opt}{tgt}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
