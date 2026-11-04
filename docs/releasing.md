# Publishing a desktop release

Update the package version, lockfile and Flatpak metadata, and write `docs/releases/vVERSION.md` before tagging. Run the normal checks and build the local bundle so the notes describe a verified version.

Create an annotated `vVERSION` tag on the release commit and push it with the branch. `Publish release` calls the existing desktop and Flatpak workflows at that same commit. After all builds pass, it downloads their artifacts, packages the native executables with the license and documentation, copies the Flatpak, and generates `SHA256SUMS`.

Only the publication job has repository-content write permission. It creates a draft, uploads every package and its checksums, then publishes it as the latest release. A failed upload leaves a draft that the workflow can resume; an already published release is not overwritten. Release packaging rejects a tag that does not match `Cargo.toml`, missing release notes, missing native platforms, empty executables or a missing/ambiguous Flatpak bundle.
