use crate::model::Channel;
use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub const REPOSITORY: &str = "https://github.com/horizon-rust/horizon";
const API: &str = "https://api.github.com/repos/horizon-rust/horizon";
const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
    pub digest: Option<String>,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub name: String,
    #[serde(default)]
    pub body: String,
    pub published_at: Option<String>,
    pub prerelease: bool,
    pub draft: bool,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug)]
pub struct Catalog {
    pub engine: Release,
    pub engine_asset: Asset,
    pub game: Release,
    pub game_asset: Asset,
}

impl Catalog {
    /// Horizon exposes a rolling GitHub tag but installs immutable date/commit versions.
    pub fn game_version(&self) -> Result<String> {
        if self.game.tag_name != "nightly" {
            return Ok(self.game.tag_name.clone());
        }
        let mut parts = self
            .game
            .name
            .strip_prefix("Nightly ")
            .context("Unknown Nightly title format")?
            .split(" · ");
        let date = parts.next().context("Missing Nightly date")?;
        ensure!(
            date.len() == 10
                && date.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }),
            "Invalid Nightly date"
        );
        match parts.next() {
            Some(commit) if !commit.is_empty() && commit.bytes().all(|b| b.is_ascii_hexdigit()) => {
                Ok(format!("nightly-{date}-{commit}"))
            }
            None => Ok(format!("nightly-{date}")),
            _ => bail!("Invalid Nightly revision"),
        }
    }
}

pub fn client() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("mcrslauncher/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .https_only(true)
        .build()
        .context("Cannot create HTTPS client")
}

pub fn platform_assets(os: &str, arch: &str) -> Result<(&'static str, &'static str)> {
    match (os, arch) {
        ("linux", "x86_64") => Ok((
            "horizon-launcher-linux-x86_64.tar.gz",
            "horizon-linux-x86_64.tar.gz",
        )),
        ("windows", "x86_64") => Ok((
            "horizon-launcher-windows-x64.zip",
            "horizon-windows-x64.zip",
        )),
        ("macos", "aarch64") => Ok((
            "horizon-launcher-macos-arm64.zip",
            "horizon-macos-arm64.zip",
        )),
        ("macos", "x86_64") => Ok(("horizon-launcher-macos-x64.zip", "horizon-macos-x64.zip")),
        _ => bail!("Horizon does not publish a supported build for {os}/{arch}"),
    }
}

pub fn select_catalog(
    releases: &[Release],
    channel: Channel,
    os: &str,
    arch: &str,
) -> Result<Catalog> {
    let (engine_name, game_name) = platform_assets(os, arch)?;
    let engine = releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease && r.assets.iter().any(|a| a.name == engine_name))
        .max_by_key(|r| &r.published_at)
        .context("No supported Horizon engine release is available")?;
    let game = releases
        .iter()
        .filter(|r| !r.draft && r.assets.iter().any(|a| a.name == game_name))
        .filter(|r| match channel {
            Channel::Nightly => r.tag_name == "nightly",
            Channel::Stable => !r.prerelease && r.tag_name != "nightly",
        })
        .max_by_key(|r| &r.published_at)
        .with_context(|| format!("No {} game release for this platform", channel.label()))?;
    Ok(Catalog {
        engine: engine.clone(),
        engine_asset: engine
            .assets
            .iter()
            .find(|a| a.name == engine_name)
            .unwrap()
            .clone(),
        game: game.clone(),
        game_asset: game
            .assets
            .iter()
            .find(|a| a.name == game_name)
            .unwrap()
            .clone(),
    })
}

pub fn fetch_catalog(client: &Client, channel: Channel) -> Result<Catalog> {
    let mut releases = Vec::new();
    // The rolling Nightly can fall behind many launcher releases in GitHub's release list.
    // Fetch its fixed tag separately; never mistake a launcher release for a game release.
    let response = client
        .get(format!("{API}/releases?per_page=100"))
        .timeout(Duration::from_secs(30))
        .send()?
        .error_for_status()
        .context("GitHub update check failed; check your connection or API rate limit")?;
    releases.extend(response.json::<Vec<Release>>()?);
    if channel == Channel::Nightly && !releases.iter().any(|r| r.tag_name == "nightly") {
        releases.push(
            client
                .get(format!("{API}/releases/tags/nightly"))
                .timeout(Duration::from_secs(30))
                .send()?
                .error_for_status()?
                .json()?,
        );
    }
    for page in 2..=4 {
        if let Ok(catalog) = select_catalog(
            &releases,
            channel,
            std::env::consts::OS,
            std::env::consts::ARCH,
        ) {
            return Ok(catalog);
        }
        let older: Vec<Release> = client
            .get(format!("{API}/releases?per_page=100&page={page}"))
            .timeout(Duration::from_secs(30))
            .send()?
            .error_for_status()?
            .json()?;
        if older.is_empty() {
            break;
        }
        releases.extend(older);
    }
    select_catalog(
        &releases,
        channel,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

pub fn digest_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn verify_digest(bytes: &[u8], digest: &str) -> Result<()> {
    let expected = digest
        .strip_prefix("sha256:")
        .context("GitHub did not provide a SHA-256 checksum")?;
    ensure!(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid SHA-256 checksum"
    );
    ensure!(
        digest_hex(bytes).eq_ignore_ascii_case(expected),
        "Download checksum mismatch; file was rejected"
    );
    Ok(())
}

pub fn download_verified(
    client: &Client,
    asset: &Asset,
    progress: impl Fn(u64, u64) -> Result<()> + Sync,
) -> Result<Vec<u8>> {
    let url = url::Url::parse(&asset.browser_download_url)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url
                .path()
                .starts_with("/horizon-rust/horizon/releases/download/"),
        "Unexpected engine download URL"
    );
    ensure!(
        asset.size > 0 && asset.size <= MAX_ARCHIVE,
        "Engine archive is empty or too large"
    );
    let checksum = asset
        .digest
        .as_deref()
        .context("Engine release has no SHA-256 digest; download refused")?;
    let bytes = if asset.size >= 4 * 1024 * 1024 {
        download_parallel(client, &url, asset.size as usize, &progress)?
    } else {
        download_stream(client, &url, asset.size, &progress)?
    };
    verify_digest(&bytes, checksum)?;
    progress(asset.size, asset.size)?;
    Ok(bytes)
}

fn download_stream(
    client: &Client,
    url: &url::Url,
    size: u64,
    progress: &(impl Fn(u64, u64) -> Result<()> + Sync),
) -> Result<Vec<u8>> {
    let mut response = client.get(url.clone()).send()?.error_for_status()?;
    let mut bytes = Vec::with_capacity(size as usize);
    let mut buffer = [0u8; 64 * 1024];
    let mut last = std::time::Instant::now();
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        ensure!(
            bytes.len() as u64 + read as u64 <= MAX_ARCHIVE,
            "Engine download exceeded the size limit"
        );
        bytes.extend_from_slice(&buffer[..read]);
        if last.elapsed() >= Duration::from_millis(100) {
            progress(bytes.len() as u64, size)?;
            last = std::time::Instant::now();
        }
    }
    ensure!(bytes.len() as u64 == size, "Engine download was incomplete");
    Ok(bytes)
}

/// Four bounded streams write directly into disjoint slices of one download buffer.
/// Servers that ignore Range fall back to one ordinary, still checksum-verified request.
fn download_parallel(
    client: &Client,
    url: &url::Url,
    size: usize,
    progress: &(impl Fn(u64, u64) -> Result<()> + Sync),
) -> Result<Vec<u8>> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let mut bytes = vec![0; size];
    let part_size = size.div_ceil(4);
    let downloaded = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let unsupported = AtomicBool::new(false);
    let result = std::thread::scope(|scope| -> Result<()> {
        let handles: Vec<_> = bytes
            .chunks_mut(part_size)
            .enumerate()
            .map(|(index, part)| {
                let downloaded = &downloaded;
                let aborted = &aborted;
                let unsupported = &unsupported;
                scope.spawn(move || -> Result<()> {
                    let result = (|| -> Result<()> {
                        let start = index * part_size;
                        let end = start + part.len() - 1;
                        let mut response = client
                            .get(url.clone())
                            .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
                            .send()?
                            .error_for_status()?;
                        if response.status() == reqwest::StatusCode::OK {
                            unsupported.store(true, Ordering::Relaxed);
                            bail!("Server does not support ranged downloads");
                        }
                        ensure!(
                            response.status() == reqwest::StatusCode::PARTIAL_CONTENT,
                            "Unexpected ranged download response"
                        );
                        let expected = format!("bytes {start}-{end}/{size}");
                        ensure!(
                            response
                                .headers()
                                .get(reqwest::header::CONTENT_RANGE)
                                .and_then(|h| h.to_str().ok())
                                == Some(expected.as_str()),
                            "Download server returned the wrong byte range"
                        );
                        let mut offset = 0;
                        let mut last = std::time::Instant::now();
                        while offset < part.len() {
                            ensure!(!aborted.load(Ordering::Relaxed), "Download stream stopped");
                            let count = response.read(&mut part[offset..])?;
                            ensure!(count > 0, "Engine download was incomplete");
                            offset += count;
                            let total = downloaded.fetch_add(count, Ordering::Relaxed) + count;
                            if last.elapsed() >= Duration::from_millis(200) {
                                progress(total as u64, size as u64)?;
                                last = std::time::Instant::now();
                            }
                        }
                        ensure!(
                            response.read(&mut [0u8; 1])? == 0,
                            "Download exceeded its byte range"
                        );
                        Ok(())
                    })();
                    if result.is_err() {
                        aborted.store(true, Ordering::Relaxed);
                    }
                    result
                })
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("Download thread stopped"))??;
        }
        Ok(())
    });
    if unsupported.load(Ordering::Relaxed) {
        progress(0, size as u64)?;
        return download_stream(client, url, size as u64, progress);
    }
    result?;
    Ok(bytes)
}

/// Extract only the launcher program. Archive paths, links and unrelated files are never used.
pub fn extract_engine(bytes: &[u8], archive_name: &str, destination: &Path) -> Result<()> {
    let expected = if cfg!(windows) {
        "horizon-launcher.exe"
    } else {
        "horizon-launcher"
    };
    let mut program = None;
    if archive_name.ends_with(".tar.gz") {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.header().entry_type().is_file()
                && entry.path()?.file_name().is_some_and(|n| n == expected)
            {
                ensure!(
                    program.is_none(),
                    "Engine archive contains duplicate executables"
                );
                let mut data = Vec::new();
                entry
                    .by_ref()
                    .take(MAX_ARCHIVE + 1)
                    .read_to_end(&mut data)?;
                ensure!(
                    data.len() as u64 <= MAX_ARCHIVE,
                    "Extracted engine is too large"
                );
                program = Some(data);
            }
        }
    } else if archive_name.ends_with(".zip") {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            if entry.is_file()
                && !entry.is_symlink()
                && Path::new(entry.name())
                    .file_name()
                    .is_some_and(|n| n == expected)
            {
                ensure!(
                    program.is_none(),
                    "Engine archive contains duplicate executables"
                );
                let mut data = Vec::new();
                entry
                    .by_ref()
                    .take(MAX_ARCHIVE + 1)
                    .read_to_end(&mut data)?;
                ensure!(
                    data.len() as u64 <= MAX_ARCHIVE,
                    "Extracted engine is too large"
                );
                program = Some(data);
            }
        }
    } else {
        bail!("Unsupported engine archive format");
    }
    let bytes = program.context("The release archive does not contain horizon-launcher")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(destination.parent().context("Missing runtime folder")?)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    temporary.persist(destination).map_err(|e| e.error)?;
    Ok(())
}

/// Accept only ordinary HTTPS GitHub repository, release, branch or commit links.
pub fn validate_mod_url(text: &str) -> Result<String> {
    let url = url::Url::parse(text.trim())
        .context("Enter a full https://github.com/owner/repository URL")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Use an HTTPS github.com repository URL without credentials or query parameters"
    );
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    ensure!(
        parts.len() >= 2
            && parts[0..2].iter().all(|p| !p.is_empty()
                && !p.starts_with('.')
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))),
        "Invalid GitHub owner or repository"
    );
    ensure!(
        parts.len() == 2
            || (parts.len() >= 4 && matches!(parts[2], "tree" | "commit"))
            || (parts.len() >= 5 && parts[2..4] == ["releases", "tag"]),
        "Use a repository, tree, commit or release tag URL"
    );
    ensure!(
        !url.path().contains('%'),
        "Percent-encoded mod links are not supported"
    );
    Ok(url.to_string().trim_end_matches('/').to_string())
}

pub fn origin_files(home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home.join("horizon-native-mods")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".origin.json"))
        })
        .collect()
}
