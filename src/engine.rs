//! A narrow, argument-based bridge to Horizon's official CLI. Account secrets and the
//! native-mod toolchain stay with upstream; this app never implements a second auth store.
use crate::{
    github::{self, Catalog},
    model::{self, Account, Mod, Paths, Receipt, Settings, Snapshot},
    network,
    progress::{FileProgress, FileTransfer, TransferMeter, TransferProgress},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum Output {
    Log(String),
    Stage(String),
    Progress(TransferProgress),
    Login { url: String, code: String },
    Playing,
}

pub type Reporter<'a> = &'a (dyn Fn(Output) + Sync);

pub struct Engine {
    pub paths: Paths,
    pub client: reqwest::blocking::Client,
    pub cancel: Arc<AtomicBool>,
}

impl Engine {
    pub fn new(paths: Paths, cancel: Arc<AtomicBool>) -> Result<Self> {
        paths.create()?;
        Ok(Self {
            paths,
            client: github::client()?,
            cancel,
        })
    }

    pub fn receipt(&self) -> Receipt {
        fs::read(self.paths.receipt())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_receipt(&self, receipt: &Receipt) -> Result<()> {
        model::atomic_write(&self.paths.receipt(), &serde_json::to_vec_pretty(receipt)?)
    }

    pub fn mark_mods_dirty(&self) -> Result<()> {
        let mut receipt = self.receipt();
        receipt.mods_dirty = true;
        self.save_receipt(&receipt)
    }

    pub fn restore_base(&self, report: Reporter<'_>) -> Result<()> {
        let mut receipt = self.receipt();
        ensure!(!receipt.game_tag.is_empty(), "No base game is installed");
        let instances = self.json(&["instance", "list"])?;
        let id = instances
            .as_array()
            .and_then(|all| all.iter().find(|i| i["selected"] == true))
            .and_then(|i| i["id"].as_str())
            .context("No selected game instance")?;
        self.run(&["instance", "version", id, "latest"], report, true)?;
        self.run(
            &["settings", "selected-version", &receipt.game_tag],
            report,
            true,
        )?;
        receipt.mods_dirty = false;
        self.save_receipt(&receipt)
    }

    pub fn verify_local_engine(&self) -> Result<()> {
        let receipt = self.receipt();
        ensure!(
            !receipt.executable_sha256.is_empty(),
            "Set up the Horizon engine first"
        );
        let bytes = fs::read(self.paths.engine())
            .context("The Horizon engine is missing; run setup again")?;
        ensure!(
            github::digest_hex(&bytes) == receipt.executable_sha256,
            "The local Horizon engine has changed; run setup to restore a verified copy"
        );
        Ok(())
    }

    pub fn catalog(&self, settings: &Settings) -> Result<Catalog> {
        self.catalog_reported(settings, &|_| {})
    }

    pub fn catalog_reported(&self, settings: &Settings, report: Reporter<'_>) -> Result<Catalog> {
        github::fetch_catalog_cancellable(
            &self.client,
            settings.channel,
            &self.cancel,
            |attempt, delay| {
                report(Output::Stage(format!(
                    "GitHub connection interrupted · retry {attempt} of 3 in {}s",
                    delay.as_secs()
                )));
            },
        )
    }

    pub fn ensure_engine(&self, catalog: &Catalog, report: Reporter<'_>) -> Result<()> {
        self.check_cancel()?;
        let mut receipt = self.receipt();
        if receipt.tag == catalog.engine.tag_name
            && Some(receipt.digest.as_str()) == catalog.engine_asset.digest.as_deref()
            && self.verify_local_engine().is_ok()
        {
            return Ok(());
        }
        report(Output::Stage(
            "Downloading the official Horizon engine".into(),
        ));
        let meter = Mutex::new(TransferMeter::default());
        let started = Instant::now();
        let bytes = network::retry(
            &self.cancel,
            &[Duration::from_secs(2), Duration::from_secs(5)],
            |attempt, delay| {
                report(Output::Stage(format!(
                    "GitHub download interrupted · retry {attempt} of 3 in {}s",
                    delay.as_secs()
                )));
            },
            || {
                github::download_verified(
                    &self.client,
                    &catalog.engine_asset,
                    &self.cancel,
                    |bytes, total| {
                        self.check_cancel()?;
                        let progress = meter.lock().unwrap_or_else(|e| e.into_inner()).observe(
                            bytes,
                            total,
                            started.elapsed(),
                        );
                        report(Output::Progress(progress));
                        Ok(())
                    },
                )
            },
        )?;
        self.check_cancel()?;
        github::extract_engine(&bytes, &catalog.engine_asset.name, &self.paths.engine())?;
        receipt.tag = catalog.engine.tag_name.clone();
        receipt.digest = catalog.engine_asset.digest.clone().unwrap_or_default();
        receipt.executable_sha256 = github::digest_hex(&fs::read(self.paths.engine())?);
        self.save_receipt(&receipt)?;
        report(Output::Log(format!(
            "Verified and installed {}",
            receipt.tag
        )));
        Ok(())
    }

    fn check_cancel(&self) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "Operation cancelled");
        Ok(())
    }

    /// Every argument is passed directly to the process, with no shell interpolation.
    pub fn run(&self, args: &[&str], report: Reporter<'_>, quiet: bool) -> Result<String> {
        self.run_process(args, report, quiet, None)
    }

    fn run_process(
        &self,
        args: &[&str],
        report: Reporter<'_>,
        quiet: bool,
        transfer: Option<FileTransfer>,
    ) -> Result<String> {
        self.check_cancel()?;
        let mut command = Command::new(self.paths.engine());
        command
            .args(args)
            .env("HORIZON_LAUNCHER_HOME", self.paths.engine_home())
            .env("HORIZON_HOME", self.paths.game_home());
        // Horizon preserves this variable when starting its managed Cargo toolchain.
        // Use every available core unless the user explicitly limits build parallelism.
        if args.first() == Some(&"native-build") && std::env::var_os("CARGO_BUILD_JOBS").is_none() {
            command.env(
                "CARGO_BUILD_JOBS",
                thread::available_parallelism()
                    .map_or(1, |cores| cores.get())
                    .to_string(),
            );
        }
        self.run_command(command, report, quiet, transfer)
    }

    fn run_command(
        &self,
        mut command: Command,
        report: Reporter<'_>,
        quiet: bool,
        mut transfer: Option<FileTransfer>,
    ) -> Result<String> {
        self.check_cancel()?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .spawn()
            .context("Cannot start Horizon; its system libraries may be missing")?;
        let stdout = child.stdout.take().context("Missing process output")?;
        let stderr = child.stderr.take().context("Missing process diagnostics")?;
        let started = Instant::now();
        let mut sampled = Instant::now();
        let (status, output) = thread::scope(|scope| -> Result<_> {
            let out = scope.spawn(|| read_output(stdout, quiet, false, report));
            let err = scope.spawn(|| read_output(stderr, quiet, true, report));
            let status = loop {
                if self.cancel.load(Ordering::Relaxed) {
                    terminate_tree(&mut child);
                    break child.wait()?;
                }
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if sampled.elapsed() >= Duration::from_millis(300) {
                    if let Some(progress) =
                        transfer.as_mut().and_then(|t| t.poll(started.elapsed()))
                    {
                        match progress {
                            FileProgress::Download(progress) => report(Output::Progress(progress)),
                            FileProgress::Unpacking => report(Output::Stage(
                                "Verifying and unpacking the game download".into(),
                            )),
                        }
                    }
                    sampled = Instant::now();
                }
                thread::sleep(Duration::from_millis(80));
            };
            let stdout = out
                .join()
                .map_err(|_| anyhow::anyhow!("Output reader stopped"))??;
            let stderr = err
                .join()
                .map_err(|_| anyhow::anyhow!("Diagnostic reader stopped"))??;
            Ok((status, (stdout, stderr)))
        })?;
        self.check_cancel()?;
        if !status.success() {
            let detail = if output.1.trim().is_empty() {
                &output.0
            } else {
                &output.1
            };
            bail!("Horizon exited with {status}: {}", tail(detail, 1800));
        }
        Ok(output.0)
    }

    pub fn json(&self, args: &[&str]) -> Result<Value> {
        let mut arguments = vec!["--json"];
        arguments.extend(args);
        let text = self.run(&arguments, &|_| {}, true)?;
        serde_json::from_str(text.trim())
            .with_context(|| format!("Unexpected response to {}", args.join(" ")))
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        if !self.paths.engine().exists() {
            return Ok(Snapshot::default());
        }
        self.verify_local_engine()?;
        let accounts = self.json(&["account", "list"])?;
        let versions = self.json(&["versions", "--installed"])?;
        let instances = self.json(&["instance", "list"])?;
        let instance = instances
            .as_array()
            .and_then(|items| items.iter().find(|i| i["selected"] == true));
        let selected_version = instance
            .and_then(|i| i["version"].as_str())
            .or_else(|| versions["selected"].as_str())
            .map(str::to_string);
        let home = instance
            .and_then(|i| i["home"].as_str())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| self.paths.game_home());
        let mut mods = Vec::new();
        for path in github::origin_files(&home) {
            let data: Value = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("Invalid mod origin: {}", path.display()))?;
            let owner = data["owner"].as_str().unwrap_or_default();
            let repo = data["repo"].as_str().unwrap_or_default();
            let commit = data["commit"].as_str().unwrap_or_default();
            mods.push(Mod {
                id: data["id"].as_str().unwrap_or("unknown").to_string(),
                version: data["version"].as_str().unwrap_or_default().to_string(),
                repository: format!("{owner}/{repo}"),
                source_url: format!("https://github.com/{owner}/{repo}/tree/{commit}"),
            });
        }
        mods.sort_by(|a, b| a.id.cmp(&b.id));
        let mut profiles = serde_json::from_value::<Vec<Account>>(accounts["accounts"].clone())?;
        // This upstream file contains public profile metadata, never authentication secrets.
        if let Ok(bytes) = fs::read(self.paths.engine_home().join("accounts.json"))
            && let Ok(saved) = serde_json::from_slice::<Value>(&bytes)
        {
            for account in &mut profiles {
                account.skin = saved["accounts"]
                    .as_array()
                    .and_then(|items| items.iter().find(|a| a["uuid"] == account.uuid))
                    .and_then(|a| serde_json::from_value(a["skin"].clone()).ok())
                    .flatten();
            }
        }
        Ok(Snapshot {
            resources_ready: selected_version
                .as_deref()
                .is_some_and(|version| self.resources_ready(version)),
            accounts: profiles,
            sign_in_configured: accounts["sign_in_configured"].as_bool().unwrap_or(false),
            owner: accounts["owner"].as_bool().unwrap_or(false),
            installed: serde_json::from_value(versions["installed"].clone())?,
            native_build: selected_version
                .as_deref()
                .is_some_and(|v| v.starts_with("native-")),
            selected_version,
            mods,
            game_home: home,
        })
    }

    pub fn install_game(
        &self,
        catalog: &Catalog,
        settings: &Settings,
        rebuild: bool,
        report: Reporter<'_>,
    ) -> Result<()> {
        self.ensure_engine(catalog, report)?;
        self.verify_local_engine()?;
        let mut receipt = self.receipt();
        let fingerprint = game_fingerprint(catalog);
        let version = catalog.game_version()?;
        let snapshot = self.snapshot()?;
        let changed = receipt.game_tag != version
            || receipt.game_digest != fingerprint
            || !snapshot.installed.contains(&version);
        if changed {
            report(Output::Stage(format!("Installing {version}")));
            let archive = self
                .paths
                .engine_home()
                .join("versions")
                .join(format!(".{version}.partial"))
                .join("download");
            self.run_process(
                &["install", &version],
                report,
                false,
                Some(FileTransfer::new(archive, catalog.game_asset.size)),
            )?;
            receipt.game_tag = version.clone();
            receipt.game_digest = fingerprint;
            self.save_receipt(&receipt)?;
        }
        if rebuild
            && !snapshot.mods.is_empty()
            && (changed || !snapshot.native_build || receipt.mods_dirty)
        {
            // Build on the catalog version just installed, even when another channel has a
            // newer installed version. Preparation and compilation must use the same base.
            self.build_version(settings, &version, report)?;
        } else if snapshot.mods.is_empty() {
            // Keep the instance following the current game rather than an old imported build.
            self.run(&["settings", "selected-version", &version], report, true)?;
        }
        self.prune_old_nightlies(&version, report);
        Ok(())
    }

    /// Horizon removes older Nightlies only from its own update path. Installing an exact tag,
    /// as this launcher does, skips that path, so those folders would accumulate forever. Remove
    /// every other Nightly that the selection or an instance does not pin, like an update does.
    fn prune_old_nightlies(&self, current: &str, report: Reporter<'_>) {
        if !is_nightly_version(current) {
            return;
        }
        let versions = match self.json(&["versions", "--installed"]) {
            Ok(value) => value,
            Err(error) => {
                report(Output::Log(format!(
                    "Could not check for old Horizon builds to remove: {error:#}"
                )));
                return;
            }
        };
        // Without both answers, an older build could still be selected or pinned. Keep
        // everything rather than risk deleting a version another instance needs.
        let settings = match self.json(&["settings"]) {
            Ok(value) => value,
            Err(error) => {
                report(Output::Log(format!(
                    "Could not check the selected Horizon build: {error:#}"
                )));
                return;
            }
        };
        let instances = match self.json(&["instance", "list"]) {
            Ok(value) => value,
            Err(error) => {
                report(Output::Log(format!(
                    "Could not check the Horizon instances: {error:#}"
                )));
                return;
            }
        };
        let mut protected = vec![current.to_string()];
        for version in [
            settings["selected_version"].as_str(),
            versions["selected"].as_str(),
        ]
        .into_iter()
        .flatten()
        {
            protected.push(version.to_string());
        }
        for instance in instances.as_array().into_iter().flatten() {
            if let Some(version) = instance["version"].as_str() {
                protected.push(version.to_string());
            }
        }
        for version in versions["installed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|version| version.as_str())
        {
            if version == current
                || !is_nightly_version(version)
                || protected.iter().any(|kept| kept == version)
            {
                continue;
            }
            report(Output::Log(format!("Removing old Horizon build {version}")));
            if let Err(error) = self.run(&["remove", version], report, true) {
                report(Output::Log(format!(
                    "Could not remove old Horizon build {version}: {error:#}"
                )));
            }
        }
    }

    pub fn build(&self, settings: &Settings, update: bool, report: Reporter<'_>) -> Result<()> {
        let snapshot = self.snapshot()?;
        let imported_base = snapshot
            .selected_version
            .as_deref()
            .filter(|version| version.starts_with("imported-"))
            .and_then(|version| {
                fs::read(
                    self.paths
                        .engine_home()
                        .join("versions")
                        .join(version)
                        .join("imported.json"),
                )
                .ok()
            })
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|record| record["base"].as_str().map(str::to_owned));
        // Mirror upstream's base selection, then pin it for both prepare and native-build.
        // A normal rebuild retains an existing native build's revision and incremental cache.
        let version = imported_base
            .as_ref()
            .or(snapshot.selected_version.as_ref())
            .filter(|version| !update && snapshot.installed.contains(version))
            .or_else(|| {
                snapshot.installed.iter().find(|version| {
                    version.starts_with("nightly-")
                        || version
                            .strip_prefix('v')
                            .is_some_and(|tag| tag.starts_with(|c: char| c.is_ascii_digit()))
                })
            })
            .context("Install Horizon before building native mods")?;
        self.build_version(settings, version, report)
    }

    fn build_version(
        &self,
        settings: &Settings,
        version: &str,
        report: Reporter<'_>,
    ) -> Result<()> {
        self.verify_local_engine()?;
        self.mark_mods_dirty()?;
        self.prepare_version(version, report)?;
        report(Output::Stage(
            "Building native mods · incremental compilation".into(),
        ));
        let args = [
            "native-build",
            "--version",
            version,
            "--trust-native-code",
            "--profile",
            settings.build_profile.argument(),
        ];
        self.run(&args, report, false)?;
        let mut receipt = self.receipt();
        receipt.mods_dirty = false;
        self.save_receipt(&receipt)?;
        Ok(())
    }

    pub fn prepare(&self, report: Reporter<'_>) -> Result<()> {
        let snapshot = self.snapshot()?;
        let version = snapshot
            .selected_version
            .context("Install Horizon before playing")?;
        self.prepare_version(&version, report)?;
        ensure!(
            self.resources_ready(&version),
            "Resource preparation did not finish all required files. Select Download & set up Horizon to retry."
        );
        Ok(())
    }

    pub fn can_play_installed(&self) -> Result<bool> {
        let snapshot = self.snapshot()?;
        Ok(snapshot.owner
            && !self.receipt().mods_dirty
            && snapshot.selected_version.as_ref().is_some_and(|version| {
                snapshot.installed.contains(version) && self.resources_ready(version)
            }))
    }

    /// Start the prepared client without invoking account renewal or downloading anything.
    pub fn play_offline(&self, settings: &Settings, report: Reporter<'_>) -> Result<()> {
        let name = settings.offline_player_name()?;
        self.verify_local_engine()?;
        ensure!(
            !self.receipt().mods_dirty,
            "Pending mod changes need an online build before offline play"
        );
        let snapshot = self.snapshot()?;
        let version = snapshot
            .selected_version
            .as_deref()
            .context("Go online to install and prepare Horizon before playing offline")?;
        ensure!(
            snapshot.installed.iter().any(|v| v == version) && self.resources_ready(version),
            "Go online to install and prepare the selected build before playing offline"
        );
        let directory = self
            .paths
            .engine_home()
            .join("versions")
            .join(version)
            .canonicalize()?;
        let binary = directory.join(if cfg!(windows) {
            "horizon-client.exe"
        } else {
            "horizon-client"
        });
        ensure!(
            binary.is_file(),
            "The installed game executable is missing. Reinstall it online."
        );
        let resources = self
            .resources_path(version)
            .context("Missing prepared resources")?
            .canonicalize()?;
        fs::create_dir_all(&snapshot.game_home)?;
        let home = snapshot.game_home.canonicalize()?;
        let mut command = Command::new(binary);
        command
            .current_dir(&home)
            .env("HORIZON_HOME", &home)
            .env_remove("HORIZON_ACCOUNT_SESSIONS")
            .env_remove("HORIZON_LAUNCHER_EXE")
            .arg("--resources")
            .arg(resources);
        if let Some(name) = name {
            command.arg("--name").arg(name);
        }
        for key in [
            "LIBGL_ALWAYS_SOFTWARE",
            "GALLIUM_DRIVER",
            "WEBKIT_DISABLE_DMABUF_RENDERER",
            "WEBKIT_DISABLE_COMPOSITING_MODE",
        ] {
            command.env_remove(key);
        }
        report(Output::Stage("Horizon is running offline".into()));
        report(Output::Playing);
        self.run_command(command, report, false, None)?;
        Ok(())
    }

    fn prepare_version(&self, version: &str, report: Reporter<'_>) -> Result<()> {
        if self.resources_ready(version) {
            return Ok(());
        }
        ensure!(single_path_component(version), "Unsafe game version");
        crate::resources::prepare(
            &self.client,
            &self.paths.engine_home(),
            &self.paths.engine_home().join("versions").join(version),
            &self.cancel,
            report,
        )?;
        self.prepare_download(
            version,
            report,
            &[Duration::from_secs(2), Duration::from_secs(5)],
        )
    }

    fn resources_path(&self, version: &str) -> Option<std::path::PathBuf> {
        if !single_path_component(version) {
            return None;
        }
        let directory = self.paths.engine_home().join("versions").join(version);
        if let Ok(bytes) = fs::read(directory.join("research-lock.json"))
            && let Ok(lock) = serde_json::from_slice::<Value>(&bytes)
            && let Some(minecraft) = lock["minecraft"]["version"].as_str()
            && single_path_component(minecraft)
        {
            return Some(directory.join(".local/horizon-client").join(minecraft));
        }
        None
    }

    fn resources_ready(&self, version: &str) -> bool {
        self.resources_path(version).is_some_and(|resources| {
            resources.join("horizon-resources.json").is_file()
                && resources.join("assets/minecraft/sounds.json").is_file()
        })
    }

    fn prepare_download(
        &self,
        version: &str,
        report: Reporter<'_>,
        retry_delays: &[Duration],
    ) -> Result<()> {
        for attempt in 0..=retry_delays.len() {
            self.check_cancel()?;
            report(Output::Stage(if attempt == 0 {
                "Preparing game resources · no sign-in required".into()
            } else {
                format!(
                    "Resuming Minecraft preparation · attempt {} of {}",
                    attempt + 1,
                    retry_delays.len() + 1
                )
            }));
            let minecraft = self.paths.engine_home().join("minecraft");
            let minecraft = minecraft
                .to_str()
                .context("The resources folder is not valid UTF-8")?;
            let error = match self.run(
                &["prepare", "--minecraft", minecraft, "--version", version],
                report,
                false,
            ) {
                Ok(_) => return Ok(()),
                Err(error) => error,
            };
            self.check_cancel()?;
            if !network::is_temporary(&error) {
                return Err(error);
            }
            let Some(delay) = retry_delays.get(attempt) else {
                return Err(error.context(format!(
                    "Minecraft setup could not reach its download servers after {} attempts. \
                     Completed downloads are saved. Check your connection, then press Play to resume.",
                    retry_delays.len() + 1
                )));
            };
            report(Output::Stage(format!(
                "Connection interrupted · retrying preparation in {}s",
                delay.as_secs()
            )));
            report(Output::Log(
                "A temporary network error interrupted preparation. Completed downloads are kept; retrying automatically."
                    .into(),
            ));
            let started = Instant::now();
            while started.elapsed() < *delay {
                self.check_cancel()?;
                thread::sleep(
                    delay
                        .saturating_sub(started.elapsed())
                        .min(Duration::from_millis(80)),
                );
            }
        }
        unreachable!("preparation returns after the final attempt")
    }
}

fn single_path_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', ':'])
}

/// Whether `version` names an immutable Nightly build (`nightly-YYYY-MM-DD[-commit]`).
fn is_nightly_version(version: &str) -> bool {
    let Some(date) = version.strip_prefix("nightly-").and_then(|v| v.get(..10)) else {
        return false;
    };
    date.bytes().enumerate().all(|(index, byte)| {
        if index == 4 || index == 7 {
            byte == b'-'
        } else {
            byte.is_ascii_digit()
        }
    })
}

pub fn game_fingerprint(catalog: &Catalog) -> String {
    catalog.game_asset.digest.clone().unwrap_or_else(|| {
        format!(
            "{}:{}",
            catalog.game_asset.updated_at, catalog.game_asset.size
        )
    })
}

pub fn parse_login(line: &str) -> Option<(String, String)> {
    let text = line.trim().strip_prefix("To sign in, open ")?;
    let (url, code) = text.split_once(" in a browser and enter the code ")?;
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "https"
        || !matches!(
            parsed.host_str(),
            Some(
                "microsoft.com"
                    | "www.microsoft.com"
                    | "login.microsoftonline.com"
                    | "login.live.com"
            )
        )
    {
        return None;
    }
    let code = code.trim();
    if code.is_empty()
        || code.len() > 32
        || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return None;
    }
    Some((url.to_string(), code.to_string()))
}

fn read_output(
    stream: impl std::io::Read,
    quiet: bool,
    stderr: bool,
    report: Reporter<'_>,
) -> Result<String> {
    let mut text = String::new();
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::with_capacity(8192);
    loop {
        bytes.clear();
        if reader.by_ref().take(8192).read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&bytes);
        if stderr && let Some((url, code)) = parse_login(&line) {
            report(Output::Login { url, code });
            continue;
        }
        if !quiet && !line.trim().is_empty() {
            report(Output::Log(line.chars().take(2048).collect()));
        }
        text.push_str(&line);
        // A long game session must not grow launcher memory without limit.
        if text.len() > 128 * 1024 {
            let start = text
                .char_indices()
                .find(|(i, _)| *i >= 64 * 1024)
                .map(|(i, _)| i)
                .unwrap_or(0);
            text.drain(..start);
        }
    }
    Ok(text)
}

fn tail(text: &str, chars: usize) -> String {
    text.chars()
        .rev()
        .take(chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn terminate_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // Each job starts a private process group. Kill Cargo/rustc/game descendants too.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .output();
    }
    let _ = child.kill();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn preparation_engine(script: &str) -> (tempfile::TempDir, Engine) {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths {
            root: directory.path().to_path_buf(),
        };
        paths.create().unwrap();
        fs::write(paths.engine(), format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(paths.engine(), fs::Permissions::from_mode(0o755)).unwrap();
        let engine = Engine::new(paths, Arc::new(AtomicBool::new(false))).unwrap();
        (directory, engine)
    }

    #[test]
    fn preparation_resumes_verified_downloads_after_a_transient_dns_failure() {
        let (_directory, engine) = preparation_engine(
            r#"if [ ! -f "$HORIZON_LAUNCHER_HOME/complete-download" ]; then
    printf cached > "$HORIZON_LAUNCHER_HOME/complete-download"
    printf 'error: Java 25 is needed; downloading Java failed: Dns Failed: Temporary failure in name resolution\n' >&2
    exit 1
fi
test "$(cat "$HORIZON_LAUNCHER_HOME/complete-download")" = cached || exit 2
printf '%s\n' "$@" > "$HORIZON_LAUNCHER_HOME/resumed-arguments""#,
        );
        let events = Mutex::new(Vec::new());
        engine
            .prepare_download(
                "test",
                &|event| events.lock().unwrap().push(event),
                &[Duration::ZERO],
            )
            .unwrap();
        assert_eq!(
            fs::read_to_string(engine.paths.engine_home().join("resumed-arguments")).unwrap(),
            format!(
                "prepare\n--minecraft\n{}\n--version\ntest\n",
                engine.paths.engine_home().join("minecraft").display()
            )
        );
        assert!(events.into_inner().unwrap().iter().any(
            |event| matches!(event, Output::Stage(stage) if stage.contains("attempt 2 of 2"))
        ));
    }

    #[test]
    fn preparation_stops_retrying_after_the_limit_and_explains_how_to_resume() {
        let (_directory, engine) = preparation_engine(
            r#"printf attempt >> "$HORIZON_LAUNCHER_HOME/attempts"
printf 'error: Dns Failed: Temporary failure in name resolution\n' >&2
exit 1"#,
        );
        let error = engine
            .prepare_download("test", &|_| {}, &[Duration::ZERO; 2])
            .unwrap_err();
        assert_eq!(
            fs::read_to_string(engine.paths.engine_home().join("attempts")).unwrap(),
            "attemptattemptattempt"
        );
        assert!(error.to_string().contains("after 3 attempts"));
        assert!(error.to_string().contains("Completed downloads are saved"));
    }

    #[test]
    fn preparation_does_not_retry_permanent_failures_or_earlier_network_warnings() {
        let (_directory, engine) = preparation_engine(
            r#"printf attempt >> "$HORIZON_LAUNCHER_HOME/attempts"
printf 'The sounds could not all be downloaded: Dns Failed: Temporary failure in name resolution\n' >&2
printf 'error: Minecraft server jar checksum mismatch\n' >&2
exit 1"#,
        );
        let error = engine
            .prepare_download("test", &|_| {}, &[Duration::ZERO; 2])
            .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
        assert_eq!(
            fs::read_to_string(engine.paths.engine_home().join("attempts")).unwrap(),
            "attempt"
        );
    }

    #[test]
    fn preparation_retry_wait_remains_cancellable() {
        let (_directory, engine) = preparation_engine(
            "printf 'error: Dns Failed: Temporary failure in name resolution\\n' >&2; exit 1",
        );
        let started = Instant::now();
        let error = engine
            .prepare_download(
                "test",
                &|event| {
                    if matches!(event, Output::Stage(stage) if stage.starts_with("Connection interrupted")) {
                        engine.cancel.store(true, Ordering::Relaxed);
                    }
                },
                &[Duration::from_secs(30)],
            )
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn reports_progress_when_the_engine_has_pipes_instead_of_a_terminal() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths {
            root: directory.path().to_path_buf(),
        };
        paths.create().unwrap();
        fs::write(paths.engine(), "#!/bin/sh\nmkdir -p \"$HORIZON_LAUNCHER_HOME/versions/.test.partial\"\nprintf 12345 > \"$HORIZON_LAUNCHER_HOME/versions/.test.partial/download\"\nsleep 1\nprintf 67890 >> \"$HORIZON_LAUNCHER_HOME/versions/.test.partial/download\"\nsleep 1\n").unwrap();
        fs::set_permissions(paths.engine(), fs::Permissions::from_mode(0o755)).unwrap();
        let engine = Engine::new(paths.clone(), Arc::new(AtomicBool::new(false))).unwrap();
        let events = Mutex::new(Vec::new());
        engine
            .run_process(
                &["install", "test"],
                &|event| events.lock().unwrap().push(event),
                false,
                Some(FileTransfer::new(
                    paths.engine_home().join("versions/.test.partial/download"),
                    10,
                )),
            )
            .unwrap();
        let events = events.into_inner().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Output::Progress(p) if p.downloaded == 5 && p.total == 10))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Output::Stage(stage) if stage.contains("unpacking")))
        );
    }
}
