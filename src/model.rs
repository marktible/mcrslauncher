use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub channel: Channel,
    pub auto_check: bool,
    pub update_before_play: bool,
    pub minimize_on_play: bool,
    pub build_profile: BuildProfile,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            channel: Channel::Nightly,
            auto_check: true,
            update_before_play: true,
            minimize_on_play: true,
            build_profile: BuildProfile::Performance,
        }
    }
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
}

#[derive(Clone, Debug, Deserialize)]
pub struct Account {
    pub name: String,
    pub uuid: String,
    #[serde(default)]
    pub selected: bool,
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
