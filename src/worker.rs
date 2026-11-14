use crate::{
    engine::{Engine, Output},
    github::{self, Catalog},
    model::{Paths, Receipt, Settings, Snapshot},
    updater::{self, LauncherRelease, UpdateOutcome},
};
use anyhow::{Result, ensure};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};

#[derive(Clone, Debug)]
pub enum Action {
    Refresh,
    CheckUpdates,
    CheckLauncherUpdate,
    InstallLauncherUpdate(Box<LauncherRelease>),
    Setup,
    RepairEngine,
    SignIn,
    SignOut(String),
    SelectAccount(String),
    InstallGame,
    Play,
    ImportMod(String),
    BuildMods,
    CheckModUpdates,
    UpdateMods,
    RemoveMod(String),
}

impl Action {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Refresh => "Refreshing library",
            Self::CheckUpdates => "Checking GitHub for updates",
            Self::CheckLauncherUpdate => "Checking for MCRS Launcher updates",
            Self::InstallLauncherUpdate(_) => "Updating MCRS Launcher",
            Self::Setup => "Setting up Horizon",
            Self::RepairEngine => "Restoring the Horizon engine",
            Self::SignIn => "Signing in with Microsoft",
            Self::SignOut(_) => "Signing out",
            Self::SelectAccount(_) => "Switching account",
            Self::InstallGame => "Updating Horizon",
            Self::Play => "Getting ready to play",
            Self::ImportMod(_) => "Importing mod from GitHub",
            Self::BuildMods => "Building native mods",
            Self::CheckModUpdates => "Checking mod updates",
            Self::UpdateMods => "Updating native mods",
            Self::RemoveMod(_) => "Removing mod",
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Output(Output),
    Snapshot(Snapshot),
    Catalog(Box<Catalog>),
    UpdateUnavailable(String),
    LauncherRelease(LauncherRelease),
    LauncherUpdated(UpdateOutcome),
    Receipt(Receipt),
    Done(std::result::Result<String, String>),
}

pub struct Worker {
    pub events: Receiver<Event>,
    commands: SyncSender<(Action, Settings)>,
    cancel: Arc<AtomicBool>,
}

impl Worker {
    pub fn new(paths: Paths, context: eframe::egui::Context) -> Self {
        let (commands, inbox) = mpsc::sync_channel::<(Action, Settings)>(1);
        let (sender, events) = mpsc::sync_channel(512);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancellation = cancel.clone();
        thread::spawn(move || {
            let send = |event: Event| {
                // Logs are expendable; completion, login and state events must reach the UI.
                if matches!(event, Event::Output(Output::Log(_))) {
                    let _ = sender.try_send(event);
                } else if sender.send(event).is_err() {
                    return;
                }
                context.request_repaint();
            };
            let engine = match Engine::new(paths, cancellation) {
                Ok(engine) => engine,
                Err(error) => {
                    send(Event::Done(Err(format!("{error:#}"))));
                    return;
                }
            };
            while let Ok((action, settings)) = inbox.recv() {
                let report = |output| send(Event::Output(output));
                let result = execute(&engine, &action, &settings, &send, &report);
                // Cancellation is consumed by the job, so the final snapshot can still load.
                engine.cancel.store(false, Ordering::Relaxed);
                if !matches!(
                    action,
                    Action::Refresh
                        | Action::CheckUpdates
                        | Action::CheckModUpdates
                        | Action::CheckLauncherUpdate
                        | Action::InstallLauncherUpdate(_)
                ) {
                    match engine.snapshot() {
                        Ok(snapshot) => send(Event::Snapshot(snapshot)),
                        Err(error) => {
                            report(Output::Log(format!("Library refresh failed: {error:#}")))
                        }
                    }
                }
                send(Event::Receipt(engine.receipt()));
                send(Event::Done(result.map_err(|e| format!("{e:#}"))));
            }
        });
        Self {
            events,
            commands,
            cancel,
        }
    }

    pub fn start(&self, action: Action, settings: Settings) -> Result<()> {
        self.cancel.store(false, Ordering::Relaxed);
        self.commands
            .try_send((action, settings))
            .map_err(|_| anyhow::anyhow!("The worker is busy or unavailable"))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn execute(
    engine: &Engine,
    action: &Action,
    settings: &Settings,
    send: &(dyn Fn(Event) + Sync),
    report: crate::engine::Reporter<'_>,
) -> Result<String> {
    execute_with_catalog(engine, action, settings, send, report, &|| {
        engine.catalog_reported(settings, report)
    })
}

fn execute_with_catalog(
    engine: &Engine,
    action: &Action,
    settings: &Settings,
    send: &(dyn Fn(Event) + Sync),
    report: crate::engine::Reporter<'_>,
    catalog: &dyn Fn() -> Result<Catalog>,
) -> Result<String> {
    // Signed-out users can play a prepared installation as a local player without
    // enabling Offline mode first or starting a sign-in/update request.
    if matches!(action, Action::Play) && !settings.offline_mode && engine.paths.engine().exists() {
        let snapshot = engine.snapshot()?;
        if !snapshot.owner && snapshot.resources_ready && !engine.receipt().mods_dirty {
            engine.play_offline(settings, report)?;
            return Ok("Offline game closed".into());
        }
    }
    if settings.offline_mode {
        match action {
            Action::Play => {
                engine.play_offline(settings, report)?;
                return Ok("Offline game closed".into());
            }
            Action::Refresh | Action::SelectAccount(_) | Action::SignOut(_) => {}
            _ => anyhow::bail!("Turn off Offline mode to use this action"),
        }
    }
    match action {
        Action::CheckLauncherUpdate => {
            let release = updater::check(&engine.client, &engine.cancel, report)?;
            let newer = release.newer_than(env!("CARGO_PKG_VERSION"))?;
            send(Event::LauncherRelease(release));
            return Ok(if newer {
                "MCRS Launcher update available"
            } else {
                "MCRS Launcher is up to date"
            }
            .into());
        }
        Action::InstallLauncherUpdate(release) => {
            let outcome = updater::install(
                &engine.paths,
                &engine.client,
                release,
                &engine.cancel,
                report,
            )?;
            let message = match &outcome {
                UpdateOutcome::Installed(_) => {
                    "MCRS Launcher updated. Restart to use the new version.".into()
                }
                UpdateOutcome::Downloaded { reason, .. } => format!(
                    "Verified update downloaded. Replace the launcher manually after closing it: {reason}"
                ),
            };
            send(Event::LauncherUpdated(outcome));
            return Ok(message);
        }
        Action::Refresh => {
            send(Event::Snapshot(engine.snapshot()?));
            return Ok("Library refreshed".into());
        }
        Action::CheckUpdates => match catalog() {
            Ok(catalog) => {
                send(Event::Catalog(Box::new(catalog)));
                return Ok("Update check complete".into());
            }
            Err(error) => {
                crate::network::check_cancel(&engine.cancel)?;
                report(Output::Log(format!("Update check unavailable: {error:#}")));
                send(Event::UpdateUnavailable(
                    "Updates are unavailable. Try Check now again when GitHub is reachable.".into(),
                ));
                return Ok("Update check unavailable · installed builds are kept".into());
            }
        },
        Action::Setup
        | Action::RepairEngine
        | Action::SignIn
        | Action::InstallGame
        | Action::Play => {
            if !matches!(action, Action::Play)
                || settings.update_before_play
                || !engine.paths.engine().exists()
            {
                report(Output::Stage("Checking the latest Horizon builds".into()));
                match catalog() {
                    Ok(catalog) => {
                        send(Event::Catalog(Box::new(catalog.clone())));
                        let update = engine.ensure_engine(&catalog, report).and_then(|_| {
                            if matches!(action, Action::Setup | Action::InstallGame | Action::Play)
                            {
                                engine.install_game(&catalog, settings, true, report)
                            } else {
                                Ok(())
                            }
                        });
                        if let Err(error) = update {
                            crate::network::check_cancel(&engine.cancel)?;
                            if matches!(action, Action::Play) && engine.can_play_installed()? {
                                report(Output::Log(format!(
                                    "Could not update Horizon; using the prepared installed build: {error:#}"
                                )));
                                send(Event::UpdateUnavailable(
                                    "Could not download the latest Horizon update. Play is using your prepared installed build."
                                        .into(),
                                ));
                            } else {
                                return Err(error.context(
                                    "Could not install the latest Horizon update. An installed, prepared build is needed to play while updates are unavailable.",
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        crate::network::check_cancel(&engine.cancel)?;
                        if matches!(action, Action::Play) && engine.can_play_installed()? {
                            report(Output::Log(format!(
                                "Update check unavailable; using the prepared installed build: {error:#}"
                            )));
                            send(Event::UpdateUnavailable(
                                "Could not check for updates. Play is using your prepared installed build."
                                    .into(),
                            ));
                        } else {
                            return Err(error.context(
                                "Could not check GitHub for updates. An installed, prepared build is needed to play while updates are unavailable.",
                            ));
                        }
                    }
                }
            }
            engine.verify_local_engine()?;
            if matches!(action, Action::RepairEngine) {
                return Ok(
                    "Horizon engine is ready. Install and prepare game resources to play.".into(),
                );
            }
            if matches!(action, Action::Setup | Action::InstallGame) {
                engine.prepare(report)?;
                return Ok("Horizon is ready. Play as Guest or an offline profile; no Microsoft sign-in is required.".into());
            }
            if matches!(action, Action::SignIn) {
                engine.run(&["account", "sign-in"], report, false)?;
                return Ok("Microsoft account connected".into());
            }
            if matches!(action, Action::Play) {
                let snapshot = engine.snapshot()?;
                if engine.receipt().mods_dirty {
                    if snapshot.mods.is_empty() {
                        engine.restore_base(report)?;
                    } else {
                        engine.build(settings, false, report)?;
                    }
                }
                engine.prepare(report)?;
                if !snapshot.owner {
                    engine.play_offline(settings, report)?;
                    return Ok("Offline game closed".into());
                }
                report(Output::Stage("Horizon is running".into()));
                report(Output::Playing);
                engine.run(&["play"], report, false)?;
                return Ok("Game closed".into());
            }
        }
        Action::SignOut(uuid) => {
            engine.verify_local_engine()?;
            engine.run(&["account", "sign-out", uuid], report, true)?;
        }
        Action::SelectAccount(uuid) => {
            engine.verify_local_engine()?;
            engine.run(&["account", "select", uuid], report, true)?;
        }
        Action::ImportMod(url) => {
            let url = github::validate_mod_url(url)?;
            engine.verify_local_engine()?;
            ensure!(
                !engine.snapshot()?.installed.is_empty(),
                "Install Horizon from the Play page before importing mods"
            );
            engine.mark_mods_dirty()?;
            engine.run(
                &["mods", "add-github", &url, "--all", "--trust-native-code"],
                report,
                false,
            )?;
            engine.build(settings, false, report)?;
        }
        Action::BuildMods => {
            engine.build(settings, false, report)?;
        }
        Action::CheckModUpdates => {
            engine.verify_local_engine()?;
            let text = engine.run(&["mods", "update-github", "--check"], report, false)?;
            return Ok(if text.trim().is_empty() {
                "Mod update check complete".into()
            } else {
                text.trim().to_string()
            });
        }
        Action::UpdateMods => {
            engine.verify_local_engine()?;
            engine.mark_mods_dirty()?;
            engine.run(
                &["mods", "update-github", "--trust-native-code"],
                report,
                false,
            )?;
            engine.build(settings, false, report)?;
        }
        Action::RemoveMod(id) => {
            engine.verify_local_engine()?;
            engine.mark_mods_dirty()?;
            engine.run(&["mods", "remove", id], report, false)?;
            if !engine.snapshot()?.mods.is_empty() {
                engine.build(settings, false, report)?;
            } else {
                engine.restore_base(report)?;
            }
        }
    }
    Ok(match action {
        Action::ImportMod(_) => "Mod imported, built and selected for play",
        Action::BuildMods => "Native build complete",
        Action::InstallGame => "Horizon is up to date",
        Action::UpdateMods => "Mods updated and rebuilt",
        Action::RemoveMod(_) => "Mod removed and game restored",
        Action::SelectAccount(_) => "Account selected",
        Action::SignOut(_) => "Account signed out",
        _ => "Done",
    }
    .into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{github, model};
    use std::{fs, os::unix::fs::PermissionsExt, sync::Mutex};

    fn installed(owner: bool, prepared: bool, dirty: bool) -> (tempfile::TempDir, Engine) {
        let temporary = tempfile::tempdir().unwrap();
        let paths = Paths {
            root: temporary.path().to_path_buf(),
        };
        paths.create().unwrap();
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = --json ]; then
    case "$2" in
        account) printf '%s\n' '{{"accounts":[],"owner":{owner},"sign_in_configured":true}}';;
        versions) printf '%s\n' '{{"installed":["native-test"],"selected":"native-test"}}';;
        instance) printf '[]\n';;
    esac
    exit 0
fi
printf '%s\n' "$1" >> "$HORIZON_LAUNCHER_HOME/commands"
test "$1" = play
"#
        );
        fs::write(paths.engine(), &script).unwrap();
        fs::set_permissions(paths.engine(), fs::Permissions::from_mode(0o755)).unwrap();
        let receipt = Receipt {
            executable_sha256: github::digest_hex(script.as_bytes()),
            mods_dirty: dirty,
            ..Receipt::default()
        };
        model::atomic_write(&paths.receipt(), &serde_json::to_vec(&receipt).unwrap()).unwrap();
        if prepared {
            let version = paths.engine_home().join("versions/native-test");
            let resources = version.join(".local/horizon-client/26.3");
            fs::create_dir_all(resources.join("assets/minecraft")).unwrap();
            fs::write(
                version.join("research-lock.json"),
                r#"{"minecraft":{"version":"26.3"}}"#,
            )
            .unwrap();
            fs::write(resources.join("horizon-resources.json"), "{}").unwrap();
            fs::write(resources.join("assets/minecraft/sounds.json"), "{}").unwrap();
        }
        let engine = Engine::new(paths, Arc::new(AtomicBool::new(false))).unwrap();
        (temporary, engine)
    }

    fn unavailable() -> Result<Catalog> {
        anyhow::bail!("GitHub: Temporary failure in name resolution")
    }

    #[test]
    fn offline_play_starts_the_prepared_client_without_accounts_or_catalog_requests() {
        let (_temporary, engine) = installed(false, true, false);
        let binary = engine
            .paths
            .engine_home()
            .join("versions/native-test/horizon-client");
        fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$@\" > offline-arguments\nprintf '%s' \"$HORIZON_HOME\" > offline-home\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let settings = Settings {
            offline_mode: true,
            ..Settings::default()
        };
        execute_with_catalog(&engine, &Action::Play, &settings, &|_| {}, &|_| {}, &|| {
            panic!("offline play must not check GitHub")
        })
        .unwrap();
        let args = fs::read_to_string(engine.paths.game_home().join("offline-arguments")).unwrap();
        assert_eq!(
            args,
            format!(
                "--resources\n{}\n",
                engine
                    .paths
                    .engine_home()
                    .join("versions/native-test/.local/horizon-client/26.3")
                    .canonicalize()
                    .unwrap()
                    .display()
            )
        );
        assert_eq!(
            fs::read_to_string(engine.paths.game_home().join("offline-home")).unwrap(),
            engine
                .paths
                .game_home()
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
        );
        assert!(!engine.paths.engine_home().join("commands").exists());
    }

    #[test]
    fn setup_installs_and_prepares_without_a_microsoft_account_or_other_launcher() {
        let (_temporary, engine) = installed(false, false, false);
        crate::resources::fixture(
            &engine.paths.engine_home(),
            &engine.paths.engine_home().join("versions/v1.0.0"),
        );
        let script = r#"#!/bin/sh
if [ "$1" = --json ]; then
    case "$2" in
        account) printf '%s\n' '{"accounts":[],"owner":false,"sign_in_configured":true}';;
        versions) if [ -f "$HORIZON_LAUNCHER_HOME/installed" ]; then printf '%s\n' '{"installed":["v1.0.0"],"selected":"v1.0.0"}'; else printf '%s\n' '{"installed":[],"selected":null}'; fi;;
        instance) printf '[]\n';;
    esac
    exit 0
fi
printf '%s\n' "$@" >> "$HORIZON_LAUNCHER_HOME/setup-commands"
case "$1" in
    install) touch "$HORIZON_LAUNCHER_HOME/installed";;
    settings) :;;
    prepare)
        test "$2" = --minecraft || exit 8
        base="$HORIZON_LAUNCHER_HOME/versions/v1.0.0/.local/horizon-client/26.3"
        mkdir -p "$base/assets/minecraft"
        printf '{}' > "$base/horizon-resources.json"
        printf '{}' > "$base/assets/minecraft/sounds.json";;
    *) exit 9;;
esac
"#;
        fs::write(engine.paths.engine(), script).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let receipt = Receipt {
            tag: "engine-test".into(),
            digest: digest.clone(),
            executable_sha256: github::digest_hex(script.as_bytes()),
            ..Receipt::default()
        };
        model::atomic_write(
            &engine.paths.receipt(),
            &serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        let asset = github::Asset {
            name: "test".into(),
            browser_download_url: String::new(),
            url: None,
            size: 1,
            digest: Some(digest),
            updated_at: String::new(),
        };
        let release = github::Release {
            tag_name: "engine-test".into(),
            name: String::new(),
            body: String::new(),
            published_at: None,
            prerelease: false,
            draft: false,
            assets: vec![asset.clone()],
        };
        let mut game = release.clone();
        game.tag_name = "v1.0.0".into();
        let catalog = Catalog {
            engine: release,
            engine_asset: asset.clone(),
            game,
            game_asset: asset,
        };
        execute_with_catalog(
            &engine,
            &Action::Setup,
            &Settings::default(),
            &|_| {},
            &|_| {},
            &|| Ok(catalog.clone()),
        )
        .unwrap();
        let commands =
            fs::read_to_string(engine.paths.engine_home().join("setup-commands")).unwrap();
        assert!(commands.contains("install\nv1.0.0\n"));
        assert!(commands.contains("prepare\n--minecraft\n"));
        assert!(!commands.contains("account"));
        assert!(!commands.contains("--download"));
        let snapshot = engine.snapshot().unwrap();
        assert!(snapshot.resources_ready);
        assert!(!snapshot.owner);
        assert!(snapshot.accounts.is_empty());
    }

    #[test]
    fn signed_out_play_uses_guest_or_selected_offline_name_without_catalog_or_authentication() {
        for named in [false, true] {
            let (_temporary, engine) = installed(false, true, false);
            let binary = engine
                .paths
                .engine_home()
                .join("versions/native-test/horizon-client");
            fs::write(
                &binary,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > guest-arguments\n",
            )
            .unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
            let mut settings = Settings::default();
            if named {
                settings.create_offline_account("Local_Player").unwrap();
            }
            // No offline toggle is needed when there is no signed-in owner.
            settings.offline_mode = false;
            execute_with_catalog(&engine, &Action::Play, &settings, &|_| {}, &|_| {}, &|| {
                panic!("signed-out Play must not request updates")
            })
            .unwrap();
            let arguments =
                fs::read_to_string(engine.paths.game_home().join("guest-arguments")).unwrap();
            assert_eq!(arguments.contains("--name\nLocal_Player\n"), named);
            assert!(!arguments.contains("session"));
            assert!(!engine.paths.engine_home().join("commands").exists());
        }
    }

    #[test]
    fn offline_mode_rejects_downloads_missing_resources_and_pending_mods() {
        let settings = Settings {
            offline_mode: true,
            ..Settings::default()
        };
        for (prepared, dirty) in [(false, false), (true, true)] {
            let (_temporary, engine) = installed(false, prepared, dirty);
            assert!(
                execute_with_catalog(
                    &engine,
                    &Action::Play,
                    &settings,
                    &|_| {},
                    &|_| {},
                    &|| panic!("offline play must not check GitHub")
                )
                .is_err()
            );
            assert!(!engine.paths.engine_home().join("commands").exists());
        }
        let (_temporary, engine) = installed(false, true, false);
        for action in [
            Action::Setup,
            Action::SignIn,
            Action::CheckUpdates,
            Action::CheckLauncherUpdate,
            Action::InstallLauncherUpdate(Box::new(LauncherRelease {
                tag: "v9.0.0".into(),
                notes: String::new(),
                asset: None,
            })),
            Action::InstallGame,
            Action::BuildMods,
            Action::CheckModUpdates,
            Action::UpdateMods,
            Action::ImportMod("test".into()),
            Action::RemoveMod("test".into()),
        ] {
            let error =
                execute_with_catalog(&engine, &action, &settings, &|_| {}, &|_| {}, &|| {
                    panic!("offline mode must not check GitHub")
                })
                .unwrap_err();
            assert!(error.to_string().contains("Turn off Offline mode"));
        }
    }

    #[test]
    fn offline_play_preserves_integrity_and_cancellation_checks() {
        let settings = Settings {
            offline_mode: true,
            ..Settings::default()
        };
        for cancelled in [false, true] {
            let (_temporary, engine) = installed(false, true, false);
            if cancelled {
                engine.cancel.store(true, Ordering::Relaxed);
            } else {
                fs::write(engine.paths.engine(), "corrupted engine").unwrap();
            }
            let error =
                execute_with_catalog(&engine, &Action::Play, &settings, &|_| {}, &|_| {}, &|| {
                    panic!("offline play must not check GitHub")
                })
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(if cancelled { "cancelled" } else { "changed" })
            );
            assert!(!engine.paths.engine_home().join("commands").exists());
        }
    }

    fn catalog_with_failed_engine_download() -> Catalog {
        let asset = github::Asset {
            name: "horizon-launcher-linux-x86_64.tar.gz".into(),
            browser_download_url:
                "https://github.com/horizon-rust/horizon/releases/download/test/engine".into(),
            url: None,
            size: 4,
            digest: None,
            updated_at: String::new(),
        };
        let release = github::Release {
            tag_name: "launcher@test".into(),
            name: "Launcher".into(),
            body: String::new(),
            published_at: Some("2026-11-09".into()),
            prerelease: false,
            draft: false,
            assets: vec![asset.clone()],
        };
        Catalog {
            engine: release.clone(),
            engine_asset: asset.clone(),
            game: release.clone(),
            game_asset: asset,
        }
    }

    #[test]
    fn failed_engine_update_still_plays_a_verified_prepared_build() {
        let (_temporary, engine) = installed(true, true, false);
        let events = Mutex::new(Vec::new());
        let outputs = Mutex::new(Vec::new());
        execute_with_catalog(
            &engine,
            &Action::Play,
            &Settings::default(),
            &|event| events.lock().unwrap().push(event),
            &|output| outputs.lock().unwrap().push(output),
            &|| Ok(catalog_with_failed_engine_download()),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(engine.paths.engine_home().join("commands")).unwrap(),
            "play\n"
        );
        assert!(
            events
                .into_inner()
                .unwrap()
                .iter()
                .any(|event| matches!(event, Event::UpdateUnavailable(_)))
        );
        assert!(
            outputs
                .into_inner()
                .unwrap()
                .iter()
                .any(|output| matches!(output, Output::Playing))
        );
    }

    #[test]
    fn failed_engine_update_does_not_bypass_preparation_or_pending_mods() {
        for (owner, prepared, dirty) in [(true, false, false), (true, true, true)] {
            let (_temporary, engine) = installed(owner, prepared, dirty);
            assert!(
                execute_with_catalog(
                    &engine,
                    &Action::Play,
                    &Settings::default(),
                    &|_| {},
                    &|_| {},
                    &|| Ok(catalog_with_failed_engine_download()),
                )
                .is_err()
            );
            assert!(!engine.paths.engine_home().join("commands").exists());
        }
    }

    #[test]
    fn explicit_install_does_not_fall_back_when_the_engine_download_fails() {
        let (_temporary, engine) = installed(true, true, false);
        assert!(
            execute_with_catalog(
                &engine,
                &Action::InstallGame,
                &Settings::default(),
                &|_| {},
                &|_| {},
                &|| Ok(catalog_with_failed_engine_download()),
            )
            .is_err()
        );
        assert!(!engine.paths.engine_home().join("commands").exists());
    }

    #[test]
    fn failed_update_check_still_plays_a_verified_prepared_build_with_account_checks() {
        let (_temporary, engine) = installed(true, true, false);
        let events = Mutex::new(Vec::new());
        let outputs = Mutex::new(Vec::new());
        execute_with_catalog(
            &engine,
            &Action::Play,
            &Settings::default(),
            &|event| events.lock().unwrap().push(event),
            &|output| outputs.lock().unwrap().push(output),
            &unavailable,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(engine.paths.engine_home().join("commands")).unwrap(),
            "play\n"
        );
        assert!(
            events
                .into_inner()
                .unwrap()
                .iter()
                .any(|event| matches!(event, Event::UpdateUnavailable(_)))
        );
        assert!(
            outputs
                .into_inner()
                .unwrap()
                .iter()
                .any(|output| matches!(output, Output::Playing))
        );
    }

    #[test]
    fn unavailable_updates_do_not_bypass_preparation_or_pending_mod_builds() {
        for (owner, prepared, dirty) in [(true, false, false), (true, true, true)] {
            let (_temporary, engine) = installed(owner, prepared, dirty);
            assert!(
                execute_with_catalog(
                    &engine,
                    &Action::Play,
                    &Settings::default(),
                    &|_| {},
                    &|_| {},
                    &unavailable,
                )
                .is_err()
            );
            assert!(!engine.paths.engine_home().join("commands").exists());
        }
    }

    #[test]
    fn unavailable_updates_do_not_bypass_engine_integrity_or_cancellation() {
        for cancelled in [false, true] {
            let (_temporary, engine) = installed(true, true, false);
            if cancelled {
                engine.cancel.store(true, Ordering::Relaxed);
            } else {
                fs::write(engine.paths.engine(), "corrupted engine").unwrap();
            }
            let error = execute_with_catalog(
                &engine,
                &Action::Play,
                &Settings::default(),
                &|_| {},
                &|_| {},
                &unavailable,
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(if cancelled { "cancelled" } else { "changed" })
            );
            assert!(!engine.paths.engine_home().join("commands").exists());
        }
    }

    #[test]
    fn manual_update_failure_is_nonfatal_and_does_not_publish_a_fresh_catalog() {
        let (_temporary, engine) = installed(true, true, false);
        let events = Mutex::new(Vec::new());
        execute_with_catalog(
            &engine,
            &Action::CheckUpdates,
            &Settings::default(),
            &|event| events.lock().unwrap().push(event),
            &|_| {},
            &unavailable,
        )
        .unwrap();
        let events = events.into_inner().unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::UpdateUnavailable(_)))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Catalog(_)))
        );
        assert!(!engine.paths.engine_home().join("commands").exists());
    }

    #[test]
    fn explicit_install_does_not_claim_success_when_updates_are_unavailable() {
        let (_temporary, engine) = installed(true, true, false);
        assert!(
            execute_with_catalog(
                &engine,
                &Action::InstallGame,
                &Settings::default(),
                &|_| {},
                &|_| {},
                &unavailable,
            )
            .is_err()
        );
        assert!(!engine.paths.engine_home().join("commands").exists());
    }
}
