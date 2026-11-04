# Publishing a desktop release

Update the package version, lockfile and Flatpak metadata, and write `docs/releases/vVERSION.md` before tagging. Run the normal checks and build the local bundle so the notes describe a verified version.

Create an annotated `vVERSION` tag on the release commit and push it with the branch. `Publish release` calls the existing desktop and Flatpak workflows at that same commit. After all builds pass, it downloads their artifacts, packages the native executables with the license and documentation, copies the Flatpak, and generates `SHA256SUMS`.

Only the publication job has repository-content write permission. It creates a draft, uploads every package and its checksums, then publishes it as the latest release. A failed upload leaves a draft that the workflow can resume; an already published release is not overwritten. Release packaging rejects a tag that does not match `Cargo.toml`, missing release notes, missing native platforms, empty executables or a missing/ambiguous Flatpak bundle.

Native Linux builds use Ubuntu 22.04 and verify that their required glibc symbols do not exceed 2.35. Keep that baseline pinned when updating runners; Flatpak uses its own SDK runtime. `Repair native Linux release` rebuilds an existing tag's Linux archive against this baseline, holds its release as a draft while replacing that archive and its checksums, and preserves the other downloads.

## Publishing Flatpak updates

After a stable GitHub release has passed verification, run:

```sh
python3 scripts/publish-flatpak-repo.py --tag vVERSION --push
```

The publisher downloads the release bundle, verifies its published SHA-256 digest, signs the app and repository metadata, creates static deltas, and publishes a generated snapshot to the `flatpak-repo` branch. Flatpak clients use its HTTPS raw-file URL, with GPG verification enabled. An optimistic Git lease prevents replacing a concurrent publication. The source branch and release tag are unchanged.

The dedicated signing directory is `~/.local/share/mcrslauncher-signing/flatpak-gnupg`; its fingerprint is stored beside it in `flatpak-key-id`. Keep a secure backup of this directory. The repository contains only its public key. The private signing key is local to the maintainer machine and is not available to GitHub Actions, so the signed-repository publish command is a separate maintainer step after the release workflow finishes.
