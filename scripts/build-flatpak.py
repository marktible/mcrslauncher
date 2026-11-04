#!/usr/bin/env python3
"""Build a local Flatpak bundle, checking its binary and native-build dependencies in the runtime."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent
PACKAGING = ROOT / "packaging/flatpak"
MANIFEST_PATH = PACKAGING / "io.github.marktible.MCRSLauncher.json"


def run(*arguments, **kwargs):
    print("+", " ".join(str(argument) for argument in arguments), flush=True)
    return subprocess.run([str(argument) for argument in arguments], check=True, cwd=ROOT, **kwargs)


def install_file(source, destination, executable=False):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(0o755 if executable else 0o644)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="Package an existing executable instead of compiling")
    parser.add_argument("--install", action="store_true", help="Install the resulting bundle for this user")
    arguments = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        parser.error("Horizon Flatpak bundles currently support Linux x86-64")
    if not shutil.which("flatpak"):
        parser.error("Install flatpak from your distribution first")
    manifest = json.loads(MANIFEST_PATH.read_text())
    app_id, runtime, branch = manifest["app-id"], manifest["runtime"], manifest["runtime-version"]
    binary = arguments.binary.resolve() if arguments.binary else ROOT / "target/release/mcrslauncher"
    if not arguments.binary:
        run("cargo", "build", "--release", "--locked", "-j", "2")
    if not binary.is_file():
        parser.error(f"Executable does not exist: {binary}")
    runtime_ref = f"{runtime}//{branch}"
    exists = subprocess.run(["flatpak", "info", runtime_ref], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
    if not exists:
        run("flatpak", "remote-add", "--user", "--if-not-exists", "flathub", "https://dl.flathub.org/repo/flathub.flatpakrepo")
        try:
            run("flatpak", "install", "--user", "--assumeyes", "--noninteractive", "--no-related", "flathub", runtime_ref)
        except subprocess.CalledProcessError:
            print("SDK download failed; retrying using the verified local cache and individual objects", flush=True)
            run("flatpak", "install", "--user", "--assumeyes", "--noninteractive", "--no-related", "--no-static-deltas", "flathub", runtime_ref)
    metainfo = PACKAGING / f"{app_id}.metainfo.xml"
    if shutil.which("appstreamcli"):
        run("appstreamcli", "validate", "--no-net", metainfo)
    output_directory = ROOT / "dist"
    output_directory.mkdir(exist_ok=True)
    (ROOT / "target").mkdir(exist_ok=True)
    output = output_directory / f"{app_id}.flatpak"
    with tempfile.TemporaryDirectory(prefix="flatpak-", dir=ROOT / "target") as temporary:
        temporary = Path(temporary)
        build, repo = temporary / "build", temporary / "repo"
        run("flatpak", "build-init", "--arch=x86_64", build, app_id, manifest["sdk"], runtime, branch)
        files = build / "files"
        install_file(binary, files / "bin/mcrslauncher", executable=True)
        install_file(PACKAGING / f"{app_id}.desktop", files / f"share/applications/{app_id}.desktop")
        install_file(ROOT / "assets/mcrslauncher.svg", files / f"share/icons/hicolor/scalable/apps/{app_id}.svg")
        install_file(metainfo, files / f"share/metainfo/{app_id}.metainfo.xml")
        install_file(ROOT / "LICENSE", files / f"share/licenses/{app_id}/LICENSE")
        # Test the host-built executable against the actual selected Flatpak glibc and loader.
        run("flatpak", "build", build, "/app/bin/mcrslauncher", "--help")
        # Horizon's managed Rust toolchain still needs the SDK's C compiler and headers.
        run("flatpak", "build", build, "sh", "-ec",
            "cc --version >/dev/null; pkg-config --exists alsa libudev; "
            "printf '#include <stdint.h>\\nint main(void) { return 0; }\\n' | cc -x c - -fsyntax-only")
        run("flatpak", "build-finish", "--command=" + manifest["command"], *manifest["finish-args"], build)
        run("flatpak", "build-export", "--arch=x86_64", repo, build, "stable")
        run("flatpak", "build-bundle", "--arch=x86_64",
            "--runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo", repo, output, app_id, "stable")
    with output.open("rb") as bundle:
        checksum = hashlib.file_digest(bundle, "sha256").hexdigest()
    checksum_path = output_directory / (output.name + ".sha256")
    checksum_path.write_text(f"{checksum}  {output.name}\n")
    print(f"Bundle: {output}\nSHA-256: {checksum}")
    if arguments.install:
        run("flatpak", "install", "--user", "--assumeyes", "--noninteractive", "--no-related", "--reinstall", output)
        run("flatpak", "run", app_id, "--help")
    print(f"Run: flatpak run {app_id}")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(f"Flatpak build stopped: command exited with status {error.returncode}") from None
