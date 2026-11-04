#!/usr/bin/env python3
"""Reject native Linux binaries that require a newer glibc than the release baseline."""
import argparse
from pathlib import Path
import re
import subprocess


def version(value):
    if not re.fullmatch(r"\d+\.\d+(?:\.\d+)?", value):
        raise ValueError(f"Invalid glibc version: {value}")
    return tuple(int(part) for part in value.split(".")) + (0,) * (3 - len(value.split(".")))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--max-glibc", default="2.35")
    args = parser.parse_args()
    limit = version(args.max_glibc)
    info = subprocess.check_output(
        ["readelf", "--version-info", "--wide", str(args.binary)], text=True
    )
    names = set(re.findall(r"\bName: (GLIBC_[A-Za-z0-9_.]+)", info))
    requirements = {}
    for name in names:
        try:
            requirements[name] = version(name.removeprefix("GLIBC_"))
        except ValueError:
            parser.error(f"Unrecognized glibc ABI requirement: {name}")
    if requirements:
        maximum = max(requirements, key=requirements.get)
        if requirements[maximum] > limit:
            parser.error(f"{args.binary} requires {maximum}; release limit is GLIBC_{args.max_glibc}")
    print(f"Linux ABI verified: no glibc requirement exceeds {args.max_glibc}")


if __name__ == "__main__":
    main()
