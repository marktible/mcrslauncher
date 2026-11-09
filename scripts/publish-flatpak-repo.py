#!/usr/bin/env python3
"""Sign a verified GitHub release for Flatpak updates; optionally publish the generated branch."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent.parent
APP = "io.github.marktible.MCRSLauncher"
PROJECT = "marktible/mcrslauncher"
BRANCH = "flatpak-repo"
URL = f"https://raw.githubusercontent.com/{PROJECT}/{BRANCH}/repo/"


def run(*arguments, cwd=None, capture=False):
    return subprocess.run(
        [str(argument) for argument in arguments], cwd=cwd, check=True,
        text=True, stdout=subprocess.PIPE if capture else None,
        env={**os.environ, "GIT_SSH_COMMAND": "ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=10"},
    )


def fetch(url):
    for attempt in range(3):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "mcrslauncher-flatpak-publisher"})
            with urllib.request.urlopen(request, timeout=40) as response:
                return response.read()
        except OSError:
            if attempt == 2:
                raise
            time.sleep(attempt + 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", default="latest")
    parser.add_argument("--site", type=Path, default=ROOT / "target/flatpak-update-publish")
    parser.add_argument("--gpg-home", type=Path, default=Path.home() / ".local/share/mcrslauncher-signing/flatpak-gnupg")
    parser.add_argument("--key", help="Signing fingerprint; defaults to flatpak-key-id next to the dedicated GPG home")
    parser.add_argument("--push", action="store_true", help="Publish the generated flatpak-repo branch using the existing Git SSH connection")
    args = parser.parse_args()
    site, key_home = args.site.resolve(), args.gpg_home.resolve()
    if site.exists() and any(site.iterdir()):
        record = site / "repository.json"
        if not record.is_file() or json.loads(record.read_text()).get("application") != APP:
            parser.error("Use an empty directory or an existing generated MCRS update repository")
    if key_home.is_relative_to(site):
        parser.error("The private signing directory must be outside the published site")
    key = args.key or (key_home.parent / "flatpak-key-id").read_text().strip()
    if not re.fullmatch(r"[A-Fa-f0-9]{40,64}", key):
        parser.error("Expected a signing-key fingerprint")
    public = subprocess.check_output(["gpg", "--homedir", str(key_home), "--batch", "--export", key])
    if not public or public != (ROOT / "packaging/flatpak/mcrslauncher.gpg").read_bytes():
        parser.error("Signing identity does not match the committed public update key")
    endpoint = "latest" if args.tag == "latest" else f"tags/{args.tag}"
    release = json.loads(fetch(f"https://api.github.com/repos/{PROJECT}/releases/{endpoint}"))
    tag = release["tag_name"]
    if release["draft"] or release["prerelease"] or not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        parser.error("Only published stable releases can enter the stable update repository")
    asset = next(asset for asset in release["assets"] if asset["name"] == APP + ".flatpak")
    if not (asset.get("digest") or "").startswith("sha256:"):
        parser.error("The release bundle must have a published SHA-256 digest")
    site.mkdir(parents=True, exist_ok=True)
    remote = run("git", "remote", "get-url", "origin", cwd=ROOT, capture=True).stdout.strip()
    revision = run("git", "ls-remote", "--heads", remote, "refs/heads/" + BRANCH, capture=True).stdout.split()
    expected = revision[0] if revision else ""
    if not (site / ".git").exists():
        run("git", "init", "--initial-branch=" + BRANCH, site)
        run("git", "remote", "add", "origin", remote, cwd=site)
        if expected:
            run("git", "fetch", "--depth=1", "origin", BRANCH, cwd=site)
            run("git", "checkout", "-B", BRANCH, "FETCH_HEAD", cwd=site)
    if expected:
        run("git", "fetch", "--depth=1", "origin", BRANCH, cwd=site)
        previous = json.loads(run("git", "show", "FETCH_HEAD:repository.json", cwd=site, capture=True).stdout)
        if previous.get("application") != APP:
            parser.error("Refusing to replace a branch that is not the generated MCRS update repository")
    record_path = site / "repository.json"
    previous = json.loads(record_path.read_text()) if record_path.exists() else {}
    if previous.get("bundle_digest") != asset["digest"] or previous.get("signing_key") != key:
        if not asset["browser_download_url"].startswith(f"https://github.com/{PROJECT}/releases/download/{tag}/"):
            parser.error("Unexpected release-download URL")
        data = fetch(asset["browser_download_url"])
        if len(data) != asset["size"] or "sha256:" + hashlib.sha256(data).hexdigest() != asset["digest"]:
            parser.error("Release bundle checksum mismatch")
        bundle = ROOT / "target/flatpak-update-input.flatpak"
        bundle.write_bytes(data)
        repo = site / "repo"
        if not (repo / "config").exists():
            run("ostree", "--repo=" + str(repo), "init", "--mode=archive-z2")
        run("flatpak", "build-import-bundle", "--gpg-sign=" + key, "--gpg-homedir=" + str(key_home), "--no-summary-index", repo, bundle)
        run("flatpak", "build-update-repo", "--gpg-sign=" + key, "--gpg-homedir=" + str(key_home),
            "--gpg-import=" + str(ROOT / "packaging/flatpak/mcrslauncher.gpg"), "--no-summary-index",
            "--title=MCRS Launcher", "--default-branch=stable", f"--homepage=https://github.com/{PROJECT}",
            "--generate-static-deltas", "--static-delta-jobs=2", "--prune", "--prune-depth=2", repo)
    for name in ["mcrslauncher.gpg", "mcrslauncher.flatpakrepo", APP + ".flatpakref"]:
        shutil.copyfile(ROOT / "packaging/flatpak" / name, site / name)
    record_path.write_text(json.dumps({"application": APP, "release": tag, "bundle_digest": asset["digest"], "signing_key": key, "url": URL}, indent=2) + "\n")
    (site / ".gitignore").write_text("/repo/.lock\n/repo/tmp/\n/repo/locks/\n")
    (site / "README.md").write_text(
        f"# MCRS Launcher Flatpak updates\n\nGenerated signed repository for {tag}.\n\n"
        "Install with:\n\n```sh\n"
        f"flatpak install --user https://raw.githubusercontent.com/{PROJECT}/{BRANCH}/{APP}.flatpakref\n"
        "```\n\nUpdate with `flatpak update`. This branch contains generated distribution files; source code is on `main`.\n"
    )
    print(f"Prepared signed repository for {tag}: {site}")
    if args.push:
        # This branch contains generated files only. An orphan snapshot keeps old binary
        # releases out of source clones; the lease prevents overwriting a concurrent publish.
        run("git", "checkout", "--quiet", "--orphan", "publish-" + uuid.uuid4().hex[:12], cwd=site)
        run("git", "rm", "--quiet", "-r", "--cached", "--ignore-unmatch", "-f", ".", cwd=site)
        run("git", "add", "-A", cwd=site)
        run("git", "-c", "user.name=Codex", "-c", "user.email=codex@openai.com", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", f"Publish signed Flatpak update for {tag}", cwd=site)
        run("git", "push", f"--force-with-lease=refs/heads/{BRANCH}:{expected}", "origin", f"HEAD:refs/heads/{BRANCH}", cwd=site)
        print("Published signed Flatpak update repository:", URL)


if __name__ == "__main__":
    main()
