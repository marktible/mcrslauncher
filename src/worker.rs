use crate::{
    engine::{Engine, Output},
    github::{self, Catalog},
    model::{Paths, Receipt, Settings, Snapshot},
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
    Setup,
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
            Self::Setup => "Setting up Horizon",
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
                    Action::Refresh | Action::CheckUpdates | Action::CheckModUpdates
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
    match action {
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
        Action::Setup | Action::SignIn | Action::InstallGame | Action::Play => {
            if !matches!(action, Action::Play)
                || settings.update_before_play
                || !engine.paths.engine().exists()
            {
                report(Output::Stage("Checking the latest Horizon builds".into()));
                match catalog() {
                    Ok(catalog) => {
                        send(Event::Catalog(Box::new(catalog.clone())));
                        engine.ensure_engine(&catalog, report)?;
                        if matches!(action, Action::InstallGame | Action::Play) {
                            engine.install_game(&catalog, settings, true, report)?;
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
            if matches!(action, Action::Setup) {
                return Ok("Horizon engine is ready. Sign in to Minecraft to continue.".into());
            }
            if matches!(action, Action::SignIn) {
                engine.run(&["account", "sign-in"], report, false)?;
                return Ok("Microsoft account connected".into());
            }
            if matches!(action, Action::Play) {
                let snapshot = engine.snapshot()?;
                ensure!(
                    snapshot.owner,
                    "Sign in with a Microsoft account that owns Minecraft: Java Edition"
                );
                if engine.receipt().mods_dirty {
                    if snapshot.mods.is_empty() {
                        engine.restore_base(report)?;
                    } else {
                        engine.build(settings, false, report)?;
                    }
                }
                engine.prepare(report)?;
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
    fn unavailable_updates_do_not_bypass_ownership_preparation_or_pending_mod_builds() {
        for (owner, prepared, dirty) in [
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
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
