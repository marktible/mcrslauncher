# Flatpak

Application ID: `io.github.marktible.MCRSLauncher` (Linux x86-64).

## Install a bundle

```sh
flatpak install --user dist/io.github.marktible.MCRSLauncher.flatpak
flatpak run io.github.marktible.MCRSLauncher
```

The application appears as **MCRS Launcher** in the desktop applications menu. Page shortcuts also work:

```sh
flatpak run io.github.marktible.MCRSLauncher --page mods
```

To replace an installed copy with a newer local bundle, use `flatpak install --user --reinstall path/to/io.github.marktible.MCRSLauncher.flatpak`. Reinstalling retains application data.

## Build a bundle locally

Install Rust, Python 3.11+ and Flatpak, then run:

```sh
python3 scripts/build-flatpak.py --install
```

This compiles the release executable, installs the matching SDK runtime for your user if necessary, checks the executable inside that runtime, checks Horizon's native compilation prerequisites, creates a bundle and SHA-256 file in `dist/`, and installs the app. Omit `--install` to create the bundle without installing it.

To package a previously compiled executable:

```sh
python3 scripts/build-flatpak.py --binary target/release/mcrslauncher
```

The local packaging path checks the host-built executable against the actual runtime loader before export. If the host compiler links against a newer glibc than the runtime provides, build from source inside the SDK instead.

## Build from source inside the SDK

The manifest supports the standard `flatpak-builder` offline build process. Install `flatpak-builder` and the SDK's Rust extension:

```sh
flatpak install --user flathub org.freedesktop.Sdk//26.08 org.freedesktop.Sdk.Extension.rust-stable//26.08
python3 scripts/flatpak-cargo-sources.py
flatpak-builder --user --force-clean --jobs=2 --repo=flatpak-repo flatpak-build packaging/flatpak/io.github.marktible.MCRSLauncher.json
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo flatpak-repo dist/io.github.marktible.MCRSLauncher.flatpak io.github.marktible.MCRSLauncher
```

`cargo-sources.json` is generated from the committed `Cargo.lock`. Every crate archive uses the lockfile's SHA-256 checksum. Build steps use `cargo --locked --offline`; they do not access the network. Regenerate sources whenever Cargo.lock changes. `python3 scripts/flatpak-cargo-sources.py --check` detects stale sources.

The manifest uses a local directory source so a private repository can be built without putting GitHub credentials into the package. Flatpak publication to Flathub or another remote repository is a separate action; producing a local bundle does not publish it.

## Runtime and sandbox

The application uses **org.freedesktop.Sdk 26.08** as its runtime. This includes a C compiler, C library headers, `pkg-config`, and ALSA/libudev development files. Horizon downloads its own pinned Rust toolchain, but needs those C build prerequisites to compile native mods. Using the SDK keeps that compilation inside the sandbox and avoids depending on compilers installed on the host. The SDK is a larger shared download on first install; it does not add a browser runtime to the launcher or change its UI rendering.

Permissions are limited to networking, display access, graphics acceleration, audio, IPC and the Secret Service keychain. The package does not grant host command execution or blanket access to the host home directory. URLs open using the desktop portal. Minecraft resources can be downloaded by Horizon for the signed-in account, so access to a host `.minecraft` folder is unnecessary.

Application data lives under `~/.var/app/io.github.marktible.MCRSLauncher/data/mcrslauncher/`, including the official engine, game versions, worlds, mods and compilation cache. Flatpak data is separate from a native MCRS installation. You can open the active data folder in Settings.

Horizon engine and game update checks still work inside the package. MCRS launcher-code updates require a new Flatpak bundle or a configured Flatpak remote; an installed local bundle is not itself an update service.

Uninstall the app with `flatpak uninstall --user io.github.marktible.MCRSLauncher`. Flatpak retains application data unless explicitly asked to remove it.
