#!/usr/bin/env python3
"""Generate checksum-pinned offline Flatpak Cargo sources using only Python 3.11+."""
import argparse
import json
from pathlib import Path
import re
import tomllib


ROOT = Path(__file__).resolve().parent.parent
DESTINATION = ROOT / "packaging/flatpak/cargo-sources.json"


def generate():
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    sources = []
    for package in sorted(lock["package"], key=lambda p: (p["name"], p["version"])):
        registry = package.get("source")
        if registry is None:
            continue
        if registry != "registry+https://github.com/rust-lang/crates.io-index":
            raise ValueError(f"Unsupported Cargo source: {registry}; vendor it explicitly")
        name, version, checksum = package["name"], package["version"], package["checksum"]
        if not re.fullmatch(r"[A-Za-z0-9_-]+", name) or not re.fullmatch(r"[A-Za-z0-9.+_-]+", version):
            raise ValueError("Invalid Cargo package name or version")
        if not re.fullmatch(r"[0-9a-f]{64}", checksum):
            raise ValueError(f"Invalid checksum for {name}")
        destination = f"cargo/vendor/{name}-{version}"
        sources.append({
            "type": "archive",
            "archive-type": "tar-gzip",
            "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
            "sha256": checksum,
            "dest": destination,
        })
        sources.append({
            "type": "inline",
            "dest": destination,
            "dest-filename": ".cargo-checksum.json",
            "contents": json.dumps({"package": checksum, "files": {}}, separators=(",", ":")),
        })
    sources.append({
        "type": "inline",
        "dest": "cargo",
        "dest-filename": "config.toml",
        "contents": '[source.crates-io]\nreplace-with = "vendored-sources"\n[source.vendored-sources]\ndirectory = "cargo/vendor"\n',
    })
    return json.dumps(sources, indent=2) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Fail if generated sources differ from Cargo.lock")
    arguments = parser.parse_args()
    generated = generate()
    if arguments.check:
        if not DESTINATION.exists() or DESTINATION.read_text() != generated:
            raise SystemExit("Flatpak Cargo sources are stale; run scripts/flatpak-cargo-sources.py")
        print("Flatpak Cargo sources match Cargo.lock")
    else:
        DESTINATION.write_text(generated)
        print(f"Wrote {DESTINATION.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
