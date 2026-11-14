use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Channel {
    Stable,
    #[default]
    Nightly,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "Stable",
            Self::Nightly => "Nightly",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildProfile {
    #[default]
    Performance,
    Release,
    Dev,
}

impl BuildProfile {
    pub fn argument(self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Release => "release",
            Self::Dev => "dev",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateCheckInterval {
    StartupOnly,
    #[default]
    Every15Minutes,
    Every30Minutes,
    EveryHour,
}

impl UpdateCheckInterval {
    pub fn label(self) -> &'static str {
        match self {
            Self::StartupOnly => "At startup only",
            Self::Every15Minutes => "At startup and every 15 minutes",
            Self::Every30Minutes => "At startup and every 30 minutes",
            Self::EveryHour => "At startup and every hour",
        }
    }

    pub fn duration(self) -> Option<Duration> {
        match self {
            Self::StartupOnly => None,
            Self::Every15Minutes => Some(Duration::from_secs(15 * 60)),
            Self::Every30Minutes => Some(Duration::from_secs(30 * 60)),
            Self::EveryHour => Some(Duration::from_secs(60 * 60)),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub offline_mode: bool,
    pub offline_accounts: Vec<String>,
    pub selected_offline_account: Option<String>,
    pub channel: Channel,
    pub auto_check: bool,
    pub update_check_interval: UpdateCheckInterval,
    pub update_before_play: bool,
    pub minimize_on_play: bool,
    pub build_profile: BuildProfile,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            offline_mode: false,
            offline_accounts: Vec::new(),
            selected_offline_account: None,
            channel: Channel::Nightly,
            auto_check: true,
            update_check_interval: UpdateCheckInterval::default(),
            update_before_play: true,
            minimize_on_play: true,
            build_profile: BuildProfile::Performance,
        }
    }
}

impl Settings {
    pub fn create_offline_account(&mut self, name: &str) -> Result<()> {
        let name = name.trim();
        validate_offline_name(name)?;
        ensure!(
            !self
                .offline_accounts
                .iter()
                .any(|saved| saved.eq_ignore_ascii_case(name)),
            "An offline account with this name already exists"
        );
        self.offline_accounts.push(name.to_owned());
        self.select_offline_account(Some(name))
    }

    pub fn select_offline_account(&mut self, name: Option<&str>) -> Result<()> {
        if let Some(name) = name {
            validate_offline_name(name)?;
            ensure!(
                self.offline_accounts.iter().any(|saved| saved == name),
                "This offline account no longer exists"
            );
        }
        self.selected_offline_account = name.map(str::to_owned);
        self.offline_mode = true;
        Ok(())
    }

    pub fn remove_offline_account(&mut self, name: &str) -> Result<()> {
        ensure!(
            self.offline_accounts.iter().any(|saved| saved == name),
            "This offline account no longer exists"
        );
        self.offline_accounts.retain(|saved| saved != name);
        if self.selected_offline_account.as_deref() == Some(name) {
            self.selected_offline_account = self.offline_accounts.first().cloned();
        }
        Ok(())
    }

    pub fn offline_player_name(&self) -> Result<Option<&str>> {
        if let Some(name) = self.selected_offline_account.as_deref() {
            validate_offline_name(name)?;
            ensure!(
                self.offline_accounts.iter().any(|saved| saved == name),
                "Select an existing offline account or Guest before playing"
            );
        }
        Ok(self.selected_offline_account.as_deref())
    }

    pub fn automatic_update_interval(&self) -> Option<Duration> {
        if self.auto_check && !self.offline_mode {
            self.update_check_interval.duration()
        } else {
            None
        }
    }
}

pub fn validate_offline_name(name: &str) -> Result<()> {
    ensure!(
        (3..=16).contains(&name.len())
            && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'),
        "Use 3–16 letters, digits, or underscores for an offline account name"
    );
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let root = if let Some(dir) = std::env::var_os("MCRSLAUNCHER_HOME") {
            PathBuf::from(dir)
        } else {
            directories::ProjectDirs::from("dev", "mcrs", "mcrslauncher")
                .context("Cannot find the application data folder")?
                .data_dir()
                .to_path_buf()
        };
        Ok(Self { root })
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    pub fn engine_home(&self) -> PathBuf {
        self.root.join("horizon-launcher")
    }
    pub fn game_home(&self) -> PathBuf {
        self.root.join("game")
    }
    pub fn runtime(&self) -> PathBuf {
        self.root.join("runtime")
    }
    pub fn engine(&self) -> PathBuf {
        self.runtime().join(if cfg!(windows) {
            "horizon-launcher.exe"
        } else {
            "horizon-launcher"
        })
    }
    pub fn receipt(&self) -> PathBuf {
        self.runtime().join("receipt.json")
    }

    pub fn create(&self) -> Result<()> {
        for dir in [
            &self.root,
            &self.engine_home(),
            &self.game_home(),
            &self.runtime(),
        ] {
            fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
        }
        Ok(())
    }

    pub fn load_settings(&self) -> Result<Settings> {
        if !self.config().exists() {
            return Ok(Settings::default());
        }
        serde_json::from_slice(&fs::read(self.config())?).context("Cannot read settings.json")
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        self.create()?;
        atomic_write(&self.config(), &serde_json::to_vec_pretty(settings)?)
    }
}

/// Replace a file on the same filesystem, so interruption cannot truncate settings or receipts.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing parent folder")?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub accounts: Vec<Account>,
    pub sign_in_configured: bool,
    pub owner: bool,
    pub installed: Vec<String>,
    pub selected_version: Option<String>,
    pub mods: Vec<Mod>,
    pub native_build: bool,
    pub game_home: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Account {
    pub name: String,
    pub uuid: String,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub skin: Option<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct Mod {
    pub id: String,
    pub version: String,
    pub repository: String,
    pub source_url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Receipt {
    pub tag: String,
    pub digest: String,
    pub executable_sha256: String,
    pub game_tag: String,
    pub game_digest: String,
    pub mods_dirty: bool,
}
