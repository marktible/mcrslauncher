# Validation — 2026-11-04

## Passed locally

- `cargo fmt --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo test --locked`: 14 tests passed; network test excluded by default.
- Native Linux release build with thin LTO and stripped symbols.
- Visual inspection of Play, Mods, Updates and Settings in the actual desktop app, using isolated application data.
- Real download and SHA-256 verification of Horizon `launcher@0.2.38`.
- Actual upstream CLI account query: `sign_in_configured: true`, no signed-in accounts, `owner: false`.
- Actual upstream CLI version query: `nightly-2026-11-04-67dad0d` was available for Linux x86-64.
- The official signed installer accepted that Nightly and started its game archive download.

The opt-in full installation test was manually interrupted during the slow game download. It therefore **did not pass** the complete installation or second-install assertions. Microsoft sign-in, resource preparation, game launch and a complete third-party native mod compilation have not been tested end to end with a Minecraft account. The tests cover native command arguments, requested build profiles, failed-build retry state, checksums, cancellation, storage and update selection without needing an account.

## Performance observations

The Linux release executable is approximately 10 MiB. Two 10-second idle samples on this machine recorded no additional process CPU ticks (reported as 0.0% of one core at the sampling resolution). Observed resident memory varied from approximately 39 to 90 MiB across desktop sessions and window resizing; this includes system graphics-driver allocations. These are local observations, not platform-wide guarantees.

The UI requests redraws on input, worker events and scheduled update checks. There is no idle animation loop. Activity history is capped at 500 lines, captured process output at 128 KiB per stream, and individual output reads at 8 KiB. Engine downloads use up to four bounded streams into one buffer, with a single-stream fallback when HTTP ranges are unsupported. Prepared resources and successful native builds are reused.

The application itself does not improve game FPS. The performance and release profiles control Horizon's native mod compilation. The development profile deliberately trades runtime speed for quicker compilation.

## GitHub repository

The Git remote is `https://github.com/marktible/mcrslauncher`. The repository is public, as chosen by its owner. Its initial commit and MIT license are preserved.

## Flatpak package

Built and installed `io.github.marktible.MCRSLauncher` for Linux x86-64 using the Freedesktop SDK 26.08 runtime. The local bundle is approximately 3.5 MiB; the installed application is approximately 10.5 MB, plus the shared SDK runtime. The SDK download initially timed out and succeeded on retry.

Verified the release executable against the actual Flatpak loader, compiled a C header probe inside the SDK, and checked ALSA/libudev development metadata. The installed runtime reported GCC 16.2.0, glibc 2.44, ALSA 1.2.16.1 and libudev 261. AppStream metadata validation passed. The generated source manifest contains 370 archives with checksums matching Cargo.lock. A source-only `flatpak-builder` build and the new GitHub Actions workflow have not been run locally; the delivered bundle uses the compiled release executable and was tested in the selected runtime.

The installed Flatpak passed its command-line smoke test and opened the Settings GUI on Wayland, with its window application ID matching the exported desktop entry. Permissions were checked after installation: network, IPC, Wayland/fallback X11, PulseAudio, graphics devices and Secret Service access. It has no blanket home/filesystem permission or host command-execution permission. The earlier limits on account sign-in, game preparation and complete native mod compilation still apply.

## Version 0.2.0: progress and player skins

All 26 current tests pass and Clippy is clean. A regression test runs a silent installer with pipes and observes real file growth, including the transition to verification/unpacking. Additional tests cover transfer speed and ETA, stale partial files, stalled transfers, active-instance folder selection, public skin metadata, PNG dimensions and hat compositing, official texture URL restrictions, and immediate cleanup/account matching for read-once session files.

Verified actual public skin-image retrieval and head rendering using an isolated, credential-free UI fixture. The Play page displays the real head, game-folder shortcut and skin button. The desktop's file-selection portal is available inside the installed Flatpak. No live skin upload was performed during testing; it requires the user to choose a PNG and apply it. Authentication tokens are absent from UI fixtures and events.

The 0.2.0 release and Flatpak bundle were rebuilt and installed, and the installed command reports the new version. Game/profile workers remain independent so loading a head or choosing a skin does not block an ongoing install. Unknown-progress phases use an activity bar and elapsed time; percentages are based on transferred bytes, rather than guessed progress.
