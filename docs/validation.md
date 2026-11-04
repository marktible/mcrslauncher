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

The source is prepared as a local Git repository. Creating and pushing the requested **private** GitHub repository requires an authenticated GitHub connection; none was available during this validation. No public repository was created.
