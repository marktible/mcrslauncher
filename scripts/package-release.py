#!/usr/bin/env python3
"""Package successful CI artifacts and generate checksums for a versioned release."""
import argparse
import hashlib
from pathlib import Path
import re
import shutil
import tarfile
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--tag", required=True)
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if args.tag != f"v{version}" or not re.fullmatch(r"v\d+\.\d+\.\d+", args.tag):
        parser.error("Release tag must match the package version")
    notes = ROOT / "docs/releases" / f"{args.tag}.md"
    if not notes.is_file():
        parser.error(f"Missing release notes: {notes}")
    artifacts, output = args.artifacts.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        parser.error("The release output directory must be empty")
    os_names = {"Linux": "linux", "Windows": "windows", "macOS": "macos"}
    arch_names = {"X64": "x86_64", "ARM64": "aarch64"}
    packaged = set()
    for artifact in sorted(artifacts.glob("mcrslauncher-*-*")):
        match = re.fullmatch(r"mcrslauncher-(Linux|Windows|macOS)-(X64|ARM64)", artifact.name)
        if not match or not artifact.is_dir():
            continue
        system, arch = match.groups()
        binary = "mcrslauncher.exe" if system == "Windows" else "mcrslauncher"
        source = artifact / binary
        if not source.is_file() or source.stat().st_size == 0:
            parser.error(f"Missing executable: {source}")
        name = f"mcrslauncher-{args.tag}-{os_names[system]}-{arch_names[arch]}"
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / name
            directory.mkdir()
            shutil.copyfile(source, directory / binary)
            (directory / binary).chmod(0o755)
            for document in [ROOT / "LICENSE", ROOT / "README.md", notes]:
                shutil.copyfile(document, directory / document.name)
            if system == "Windows":
                with zipfile.ZipFile(output / f"{name}.zip", "w", zipfile.ZIP_DEFLATED) as archive:
                    for file in sorted(directory.iterdir()):
                        archive.write(file, f"{name}/{file.name}")
            else:
                with tarfile.open(output / f"{name}.tar.gz", "w:gz") as archive:
                    archive.add(directory, arcname=name)
        packaged.add(system)
    if packaged != set(os_names):
        parser.error(f"Expected Linux, Windows and macOS artifacts; found {sorted(packaged)}")
    bundles = list(artifacts.rglob("io.github.marktible.MCRSLauncher.flatpak"))
    if len(bundles) != 1 or bundles[0].stat().st_size == 0:
        parser.error("Expected exactly one successful Flatpak bundle")
    shutil.copyfile(bundles[0], output / bundles[0].name)
    files = sorted(output.iterdir())
    checksums = []
    for file in files:
        with file.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        checksums.append(f"{digest}  {file.name}\n")
    (output / "SHA256SUMS").write_text("".join(checksums))
    for file in sorted(output.iterdir()):
        print(file.name)


if __name__ == "__main__":
    main()
