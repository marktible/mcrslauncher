# Publishing a desktop release

Update the package version, lockfile and Flatpak metadata, and write `docs/releases/vVERSION.md` before tagging. Run the normal checks and build the local bundle so the notes describe a verified version.

Create an annotated `vVERSION` tag on the release commit and push it with the branch. `Publish release` calls the existing desktop and Flatpak workflows at that same commit. After all builds pass, it downloads their artifacts, packages the native executables with the license and documentation, copies the Flatpak, and generates `SHA256SUMS`.

Only the publication job has repository-content write permission. It creates a draft, uploads every package and its checksums, then publishes it as the latest release. A failed upload leaves a draft that the workflow can resume; an already published release is not overwritten. Release packaging rejects a tag that does not match `Cargo.toml`, missing release notes, missing native platforms, empty executables or a missing/ambiguous Flatpak bundle.

Native Linux builds use Ubuntu 22.04 and verify that their required glibc symbols do not exceed 2.35. Keep that baseline pinned when updating runners; Flatpak uses its own SDK runtime. `Repair native Linux release` rebuilds an existing tag's Linux archive against this baseline, holds its release as a draft while replacing that archive and its checksums, and preserves the other downloads.

The release retry workflow retries only a Flatpak builder crash reported as `/app/bin/xvfb-run` exit code 139, with at most three total attempts. Other failures require review. It reruns failed jobs on the original release commit and keeps successful platform builds. A manual run checks the release for the current package version.

## Publishing Flatpak updates

After a stable GitHub release has passed verification, run:

```sh
python3 scripts/publish-flatpak-repo.py --tag vVERSION --push
```

A local rebuild is not a published Flatpak update. If the installed local commit is newer than the repository's published build, Flatpak reports “Update is older than current version.” Publish a newer numbered release and its signed repository, then verify a normal `flatpak update` against that installation before reporting the update as available.

The publisher downloads the release bundle, verifies its published SHA-256 digest, signs the app and repository metadata, creates static deltas, and publishes a generated snapshot to the `flatpak-repo` branch. Flatpak clients use its HTTPS raw-file URL, with GPG verification enabled. An optimistic Git lease prevents replacing a concurrent publication. The source branch and release tag are unchanged.

The dedicated signing directory is `~/.local/share/mcrslauncher-signing/flatpak-gnupg`; its fingerprint is stored beside it in `flatpak-key-id`. Keep a secure backup of this directory. The repository contains only its public key. The private signing key is local to the maintainer machine and is not available to GitHub Actions, so the signed-repository publish command is a separate maintainer step after the release workflow finishes.

The active signing key is the original project identity, `2D491F55DAA3468A6B08E8186B2852C4283B0388`, restored from the maintainer's backup. The restored backup is protected at `~/.local/share/mcrslauncher-signing/restored-original-key/`; the active GPG home remains `flatpak-gnupg` beside it. Both installation descriptors embed the original public key, so existing installations keep their trusted identity.

The temporary replacement key `A1EAF8BB930E352D62BD249C9A3247C5F31B2652` is retained locally as a compatibility signer. The publisher reads optional co-signing fingerprints from `flatpak-additional-key-ids` beside the GPG home; `--additional-key` can add fingerprints for a single publication. Every configured key signs both the app commit and repository summary. Keep private keys and revocation certificates outside the repository and release downloads.
