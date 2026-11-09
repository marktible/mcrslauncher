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

## Version 0.2.1: preparation animation

Inspected the user's running Flatpak during resource preparation: its file count advanced to 3,446 of 5,147, and a three-second process I/O sample recorded 849,998 bytes written. The preparation worker was active; the animation's 250 ms repaint timer caused its visible stepping.

The indeterminate bar now requests a frame every 16.7 ms while visible, independently of resource-download log messages. Minimized windows do not request animation frames. Measured download progress still repaints on worker events, with a one-second timer for its elapsed clock. Idle and game-running screens retain their event-driven rendering. All 26 tests pass, and formatting, Clippy and the Flatpak source-lock check pass.

Rebuilt and installed the 0.2.1 Flatpak, validated its metadata and runtime dependencies, and confirmed its installed command reports 0.2.1. The existing launcher and its preparation subprocess remained running across installation; the animation fix takes effect on the next launcher start.

## Version 0.2.2: resumable preparation after network failures

The user's first preparation failed during transient DNS failures for Mojang's resource and Java hosts. Host and Flatpak DNS subsequently resolved those hosts, and the two reported download URLs returned HTTP 200. Resumed the verified official engine's preparation inside Flatpak with the existing cache. The sound stage completed without a warning, Mojang's Java finished downloading, and Horizon completed its exporters and reported `v0.2.97 is prepared` with exit status 0. Verified both prepared-resource markers and ran the managed Java inside Flatpak: Microsoft OpenJDK 25.0.1. Resource preparation with the user's signed-in account is now verified; the earlier limitation on this step no longer applies. This did not launch or validate gameplay.

Preparation now makes up to three attempts for temporary DNS, connection and HTTP gateway/service failures, with two- and five-second cancellable waits. Existing downloads remain under the official engine's checksum verification. The final CLI error is classified separately from earlier nonfatal sound warnings; other failures retain their original diagnostics. Exhausted retries explain that downloads are saved and Play resumes setup.

All 30 tests pass, including new regressions for cache reuse after DNS failure, retry exhaustion, permanent failures following earlier network warnings, and cancellation during backoff. Formatting, Clippy and the Flatpak source-lock check pass.

Rebuilt and installed the 0.2.2 Flatpak, verified the bundle checksum and runtime dependencies, and confirmed its installed command reports 0.2.2.

## Version 0.2.3: prepare native-mod build inputs

The user imported shader mods after moving from a prepared stable release to an unprepared Nightly. Native compilation ran before preparation and failed at Horizon's `need_prepared` gate. Preparation is now a prerequisite of the shared build path used by import, rebuild, update, removal and Play. The same explicit `--version` is passed to preparation and compilation. Ordinary rebuilds retain the selected native revision; game installation builds on its exact catalog version even when another channel has a newer installed version.

All 34 tests pass, and formatting, Clippy and the Flatpak source-lock check pass. New regressions cover preparation before compilation, failed preparation preserving pending changes and blocking compilation, reuse of prepared native builds, and stable catalog builds with an installed Nightly.

Prepared the actual `nightly-2026-11-04-67dad0d` inside Flatpak using the existing Minecraft cache and managed Java. The official preparer completed successfully in 23.4 seconds. Retried the user's two already-imported shader bundles; the complete first build, including managed Rust 1.99.1 and dependency downloads, succeeded in 13 minutes 52 seconds. Horizon installed and selected `native-67dad0d-aa1c023f6ee0`, containing both shader installer crates at version 1.1.1. A repeat build reported up to date and reused the same artifact in 14 seconds without compiling. Native mod compilation inside Flatpak is now verified; the earlier limitation on this step no longer applies. This does not establish in-game shader behavior.

Verified both installed bundles' content digests against `native-mods.json` and the completed build's resource markers, then synchronized the launcher's pending-build flag with the successful official CLI build. Rebuilt and installed the 0.2.3 Flatpak, verified its checksum and runtime dependencies, and reopened its Mods page.

## Version 0.2.4: unavailable update checks

The user's GitHub release query failed during a DNS outage. Update metadata requests now use bounded request timeouts and up to three attempts for temporary DNS, connection and HTTP 502–504 failures, with one- and two-second cancellable waits. API rejection, rate-limit responses and malformed JSON are not retried. The network-error classifier is shared with preparation retries.

Failed update checks emit an informational update-unavailable state and retain existing in-memory release details as last-known data. They do not publish a fresh catalog or display an up-to-date claim. Play may proceed with the selected installed version when the engine checksum, resource markers, ownership and pending-mod state permit it; the normal Horizon account and launch checks still run. Cancellation is preserved, and explicit install requests still require a successful catalog query.

All 43 tests pass, including local HTTP tests for recovery from 503, rejection of invalid/rate-limited responses and retry cancellation, plus worker tests for actual Play dispatch after a failed catalog query. Negative cases cover ownership, missing resources, pending mod changes, corrupted engines, cancellation and explicit-install failure. Formatting, Clippy and the Flatpak source-lock check pass.

The live GitHub releases endpoint returned HTTP 200 after the reported outage. Visually verified the real 0.2.4 Updates page with a deliberately refused proxy connection in an isolated, credential-free fixture: it showed the informational warning, installed version and retry button without a fatal banner. Rebuilt and installed the 0.2.4 Flatpak and confirmed its installed command reports 0.2.4. The user's existing launcher and release-profile native compilation were left running; the new interface and behavior apply on the next launcher start.

## Version 0.2.5: resizable desktop layout

Reduced the window minimum from 940×640 to 480×360. Narrow windows use a compact top navigation bar, smaller margins and stacked Play cards; wide windows use an adjustable sidebar and two columns when space permits. Mod, account, update and storage actions wrap to fit. Short Play windows prioritize the launch button over the decorative landscape. Long error messages scroll with page content, the activity drawer is height-limited, and account/skin/confirmation dialogs support resizing and vertical scrolling within the viewport.

All 43 existing tests pass; formatting, Clippy and the Flatpak source-lock check pass. Visually checked all four real desktop pages at 480 pixels wide with isolated account/mod metadata, including a long player name and native version. Also checked Play at 700×700, 1140×780 and 1140×360, and Mods at 480×360. The resize checks caught and resolved inherited horizontal card layout and overlapping sidebar footer text before packaging.

Confirmed the final Play layout keeps its launch button visible at 480×360 and 1140×360. Rebuilt and installed the 0.2.5 Flatpak, verified the bundle checksum and runtime dependencies, and confirmed its installed command reports 0.2.5.

## Signed Flatpak update repository

Published the verified v0.2.5 release bundle as a signed OSTree repository on the generated `flatpak-repo` branch, served over GitHub's raw HTTPS endpoint. App and repository metadata are signed with a dedicated project key. Only the public key is committed; the maintainer's private key remains in a separate local GPG directory. The publishing script verifies the GitHub asset's size and SHA-256 digest, rejects unrelated nonempty output directories, and uses a Git lease when replacing the generated snapshot.

An isolated OSTree client rejected the repository without the trusted public key, then successfully verified and pulled it after importing that key. The actual user installation was migrated from its standalone-bundle origin to `mcrslauncher`. Both `gpg-verify` and `gpg-verify-summary` are enabled. The installed command still reports 0.2.5, and `flatpak update --user --assumeyes --noninteractive io.github.marktible.MCRSLauncher` succeeds with the current version up to date. Standard `flatpak update` includes matching per-user refs as well as system refs.

Future GitHub releases require the documented local signed-repository publication step before Flatpak clients see the update. The private signing key has not been placed in GitHub Actions, so that publication is a maintainer operation rather than an automatic part of the release workflow.

## Version 0.2.6: engine download fallback

The user's setup failed with `HTTP status server error (504 Gateway Time-out)` for the official `launcher@0.2.57` Linux archive. The release asset exists: the GitHub API reported the expected size and SHA-256 digest. While the browser download endpoint returned HTTP 504 for the archive, the GitHub release-asset API returned a signed redirect and served the same file.

Engine downloads now retry temporary HTTP 502–504, timeout and connection failures up to three attempts with cancellable two- and five-second waits. If the browser endpoint still fails, the launcher retries through the exact upstream release-asset API URL from the same verified API response, sending `Accept: application/octet-stream`. Every source must pass the published SHA-256 check before installation; the fallback is never used to bypass verification.

Downloaded the real `launcher@0.2.57` archive through the fallback path: 29,109,458 bytes matched the published SHA-256 digest. All 49 tests pass, including a local HTTP regression for a 504 followed by a successful fallback source and validation that only the upstream asset API path is accepted. Formatting and Clippy pass.

For Play, a failed engine or game update no longer blocks an already verified, prepared installation. The launcher reports an informational update-unavailable state and launches the installed build after the normal ownership, checksum, prepared-resource and pending-mod checks. Explicit setup or install requests still fail rather than claiming the update succeeded.
