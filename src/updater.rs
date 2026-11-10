//! MCRS updates are independent of the Horizon engine and game catalog.
use crate::{
    engine::{Output, Reporter},
    github::{self, Asset, Release},
    model::Paths,
    network,
    progress::TransferMeter,
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

pub const REPOSITORY: &str = "marktible/mcrslauncher";
pub const RELEASES_URL: &str = "https://github.com/marktible/mcrslauncher/releases/latest";
pub const FLATPAK_ID: &str = "io.github.marktible.MCRSLauncher";
pub const FLATPAK_UPDATE_COMMAND: &str = "flatpak update io.github.marktible.MCRSLauncher";

#[derive(Clone, Debug)]
pub struct LauncherRelease {
    pub tag: String,
    pub notes: String,
    pub asset: Option<Asset>,
}

impl LauncherRelease {
    pub fn newer_than(&self, current: &str) -> Result<bool> {
        Ok(version(&self.tag)? > version(current)?)
    }
}

fn version(text: &str) -> Result<[u64; 3]> {
    let parts: Vec<_> = text.strip_prefix('v').unwrap_or(text).split('.').collect();
    ensure!(
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "Invalid launcher release version"
    );
    Ok([parts[0].parse()?, parts[1].parse()?, parts[2].parse()?])
}

pub fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists()
}

fn asset_name(tag: &str, os: &str, arch: &str) -> Result<String> {
    version(tag)?;
    ensure!(
        matches!(
            (os, arch),
            ("linux" | "windows", "x86_64") | ("macos", "aarch64" | "x86_64")
        ),
        "No native launcher update for {os}/{arch}. Download a supported package from Releases."
    );
    let extension = if os == "windows" { "zip" } else { "tar.gz" };
    Ok(format!("mcrslauncher-{tag}-{os}-{arch}.{extension}"))
}

fn select_release(release: Release, os: &str, arch: &str) -> Result<LauncherRelease> {
    ensure!(
        !release.draft && !release.prerelease,
        "The launcher update is not a stable published release"
    );
    version(&release.tag_name)?;
    let name = asset_name(&release.tag_name, os, arch).ok();
    let asset = release
        .assets
        .into_iter()
        .find(|asset| Some(&asset.name) == name.as_ref());
    Ok(LauncherRelease {
        tag: release.tag_name,
        notes: release.body,
        asset,
    })
}

pub fn check(
    client: &reqwest::blocking::Client,
    cancel: &AtomicBool,
    report: Reporter<'_>,
) -> Result<LauncherRelease> {
    network::retry(
        cancel,
        &[Duration::from_secs(1), Duration::from_secs(2)],
        |attempt, _| {
            report(Output::Stage(format!(
                "Retrying MCRS update check · attempt {attempt} of 3"
            )));
        },
        || {
            let release = client
                .get(format!(
                    "https://api.github.com/repos/{REPOSITORY}/releases/latest"
                ))
                .timeout(Duration::from_secs(12))
                .send()?
                .error_for_status()?
                .json()?;
            select_release(release, std::env::consts::OS, std::env::consts::ARCH)
        },
    )
}

#[derive(Clone, Debug)]
pub enum UpdateOutcome {
    Installed(PathBuf),
    Downloaded { binary: PathBuf, reason: String },
}

/// Write on the destination filesystem and commit with one rename. Failed writes leave
/// the running executable intact. Windows keeps the download for replacement after exit.
fn replace_binary(source: &Path, destination: &Path, cancel: &AtomicBool) -> Result<()> {
    network::check_cancel(cancel)?;
    ensure!(
        !cfg!(windows),
        "Close MCRS and replace mcrslauncher.exe with the downloaded copy"
    );
    let parent = destination
        .parent()
        .context("Missing launcher installation folder")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .context("The launcher installation folder is not writable")?;
    let mut input = fs::File::open(source)?;
    std::io::copy(&mut input, &mut temporary)?;
    temporary.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    temporary.as_file().sync_all()?;
    network::check_cancel(cancel)?;
    temporary
        .persist(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

pub fn install(
    paths: &Paths,
    client: &reqwest::blocking::Client,
    release: &LauncherRelease,
    cancel: &AtomicBool,
    report: Reporter<'_>,
) -> Result<UpdateOutcome> {
    ensure!(
        !is_flatpak(),
        "Use the system software updater for this Flatpak installation"
    );
    ensure!(
        release.newer_than(env!("CARGO_PKG_VERSION"))?,
        "This launcher release is already installed or older"
    );
    let asset = release.asset.as_ref().context(
        "No native update package is available for this platform. Open Releases to download it.",
    )?;
    ensure!(
        asset.name == asset_name(&release.tag, std::env::consts::OS, std::env::consts::ARCH)?,
        "The launcher package does not match this platform"
    );
    report(Output::Stage(format!(
        "Downloading MCRS Launcher {}",
        release.tag
    )));
    let meter = Mutex::new(TransferMeter::default());
    let started = Instant::now();
    let bytes = network::retry(
        cancel,
        &[Duration::from_secs(1), Duration::from_secs(2)],
        |attempt, _| {
            report(Output::Stage(format!(
                "Retrying MCRS download · attempt {attempt} of 3"
            )));
        },
        || {
            github::download_verified_from_repo(
                client,
                asset,
                REPOSITORY,
                cancel,
                |downloaded, total| {
                    network::check_cancel(cancel)?;
                    let progress = meter.lock().unwrap_or_else(|e| e.into_inner()).observe(
                        downloaded,
                        total,
                        started.elapsed(),
                    );
                    report(Output::Progress(progress));
                    Ok(())
                },
            )
        },
    )?;
    network::check_cancel(cancel)?;
    let folder = paths.root.join("updates");
    fs::create_dir_all(&folder)?;
    let staging = tempfile::tempdir_in(folder)?;
    let binary = staging.path().join(if cfg!(windows) {
        "mcrslauncher.exe"
    } else {
        "mcrslauncher"
    });
    github::extract_program(
        &bytes,
        &asset.name,
        binary.file_name().unwrap().to_str().unwrap(),
        &binary,
    )?;
    network::check_cancel(cancel)?;
    let destination = std::env::current_exe()?.canonicalize()?;
    report(Output::Stage(
        "Installing the verified launcher update".into(),
    ));
    match replace_binary(&binary, &destination, cancel) {
        Ok(()) => Ok(UpdateOutcome::Installed(destination)),
        Err(error) => {
            network::check_cancel(cancel)?;
            let _ = staging.keep();
            Ok(UpdateOutcome::Downloaded {
                binary,
                reason: format!("{error:#}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_numeric_and_only_stable_package_versions_are_accepted() {
        let release = LauncherRelease {
            tag: "v0.2.10".into(),
            notes: String::new(),
            asset: None,
        };
        assert!(release.newer_than("0.2.9").unwrap());
        assert!(!release.newer_than("0.2.10").unwrap());
        assert!(!release.newer_than("1.0.0").unwrap());
        for invalid in ["nightly", "v1.2", "v1.2.3-beta", "../1.2.3", "1.2.3.4"] {
            assert!(version(invalid).is_err());
        }
        assert_eq!(
            asset_name("v0.2.10", "linux", "x86_64").unwrap(),
            "mcrslauncher-v0.2.10-linux-x86_64.tar.gz"
        );
        assert_eq!(
            asset_name("v0.2.10", "windows", "x86_64").unwrap(),
            "mcrslauncher-v0.2.10-windows-x86_64.zip"
        );
        assert!(asset_name("v0.2.10", "linux", "aarch64").is_err());
    }

    #[test]
    fn selects_only_the_launcher_package_for_the_running_platform() {
        let make = |prerelease| Release {
            tag_name: "v1.2.3".into(),
            name: "MCRS".into(),
            body: "Notes".into(),
            published_at: None,
            draft: false,
            prerelease,
            assets: vec![Asset {
                name: "mcrslauncher-v1.2.3-linux-x86_64.tar.gz".into(),
                browser_download_url: String::new(),
                url: None,
                size: 1,
                digest: None,
                updated_at: String::new(),
            }],
        };
        assert!(
            select_release(make(false), "linux", "x86_64")
                .unwrap()
                .asset
                .is_some()
        );
        assert!(
            select_release(make(false), "windows", "x86_64")
                .unwrap()
                .asset
                .is_none()
        );
        assert!(select_release(make(true), "linux", "x86_64").is_err());
    }

    #[test]
    fn extraction_keeps_only_the_launcher_and_rejects_duplicates_without_replacing_it() {
        use zip::write::SimpleFileOptions;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("launcher");
        let archive = |duplicate| {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            for name in ["../unrelated", "package/mcrslauncher"] {
                writer
                    .start_file(name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"verified executable").unwrap();
            }
            if duplicate {
                writer
                    .start_file("another/mcrslauncher", SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"unexpected executable").unwrap();
            }
            writer.finish().unwrap().into_inner()
        };
        github::extract_program(&archive(false), "release.zip", "mcrslauncher", &destination)
            .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"verified executable");
        assert!(
            !directory
                .path()
                .parent()
                .unwrap()
                .join("unrelated")
                .exists()
        );
        assert!(
            github::extract_program(&archive(true), "release.zip", "mcrslauncher", &destination)
                .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"verified executable");
    }

    #[test]
    #[cfg(unix)]
    fn replacement_is_atomic_and_failed_input_preserves_the_installed_binary() {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("launcher");
        let source = folder.path().join("new");
        fs::write(&destination, b"old").unwrap();
        let cancel = AtomicBool::new(false);
        assert!(replace_binary(&source, &destination, &cancel).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        fs::write(&source, b"new").unwrap();
        assert!(replace_binary(&source, &destination, &AtomicBool::new(true)).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        replace_binary(&source, &destination, &cancel).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
