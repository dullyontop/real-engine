#!/usr/bin/env python3
"""Print the dependency requirements of crate versions from the crates.io API.

Usage: python3 -I deps.py egui-wgpu@0.36.2 egui-winit@0.36.2 [--filter wgpu,winit]
Also prints the version's rust_version (MSRV), license and features (names only).
"""
import json, sys, time, urllib.request

UA = "real-engine-dev-tools/0.1 (render-spike deps.py; noreply@anthropic.com)"

def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)

def main(argv):
    flt = None
    specs = []
    i = 0
    while i < len(argv):
        if argv[i] == "--filter":
            flt = set(argv[i + 1].split(","))
            i += 2
            continue
        specs.append(argv[i]); i += 1
    for n, spec in enumerate(specs):
        if n:
            time.sleep(1)
        name, ver = spec.split("@")
        v = get(f"https://crates.io/api/v1/crates/{name}/{ver}")["version"]
        time.sleep(1)
        d = get(f"https://crates.io/api/v1/crates/{name}/{ver}/dependencies")["dependencies"]
        print(f"== {name} {ver}: msrv={v.get('rust_version')} license={v.get('license')} created={v.get('created_at','')[:10]} yanked={v.get('yanked')}")
        feats = v.get("features") or {}
        print(f"   features: {', '.join(sorted(feats.keys()))}")
        if "default" in feats:
            print(f"   default = {feats['default']}")
        for dep in sorted(d, key=lambda x: (x['kind'], x['crate_id'])):
            if flt and dep['crate_id'] not in flt:
                continue
            extra = []
            if dep.get('optional'): extra.append('optional')
            if not dep.get('default_features', True): extra.append('no-default-features')
            if dep.get('features'): extra.append('features=' + ','.join(dep['features']))
            if dep.get('target'): extra.append('target=' + dep['target'])
            print(f"   [{dep['kind']}] {dep['crate_id']} {dep['req']} {' '.join(extra)}")
    return 0

if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
