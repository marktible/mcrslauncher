use crate::{model::Channel, network};
use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

pub const REPOSITORY: &str = "https://github.com/horizon-rust/horizon";
const API: &str = "https://api.github.com/repos/horizon-rust/horizon";
const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub url: Option<String>,
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
    fetch_catalog_cancellable(client, channel, &AtomicBool::new(false), |_, _| {})
}

pub fn fetch_catalog_cancellable(
    client: &Client,
    channel: Channel,
    cancel: &AtomicBool,
    retrying: impl Fn(usize, Duration),
) -> Result<Catalog> {
    fetch_catalog_at(
        client,
        channel,
        API,
        cancel,
        &[Duration::from_secs(1), Duration::from_secs(2)],
        retrying,
    )
}

fn fetch_catalog_at(
    client: &Client,
    channel: Channel,
    api: &str,
    cancel: &AtomicBool,
    delays: &[Duration],
    retrying: impl Fn(usize, Duration),
) -> Result<Catalog> {
    network::retry(cancel, delays, retrying, || {
        fetch_catalog_once(client, channel, api, cancel)
    })
}

fn fetch_catalog_once(
    client: &Client,
    channel: Channel,
    api: &str,
    cancel: &AtomicBool,
) -> Result<Catalog> {
    let mut releases = Vec::new();
    // The rolling Nightly can fall behind many launcher releases in GitHub's release list.
    // Fetch its fixed tag separately; never mistake a launcher release for a game release.
    let response = client
        .get(format!("{api}/releases?per_page=100"))
        .timeout(Duration::from_secs(12))
        .send()?
        .error_for_status()
        .context("GitHub update check failed; check your connection or API rate limit")?;
    releases.extend(response.json::<Vec<Release>>()?);
    network::check_cancel(cancel)?;
    if channel == Channel::Nightly && !releases.iter().any(|r| r.tag_name == "nightly") {
        releases.push(
            client
                .get(format!("{api}/releases/tags/nightly"))
                .timeout(Duration::from_secs(12))
                .send()?
                .error_for_status()?
                .json()?,
        );
    }
    for page in 2..=4 {
        network::check_cancel(cancel)?;
        if let Ok(catalog) = select_catalog(
            &releases,
            channel,
            std::env::consts::OS,
            std::env::consts::ARCH,
        ) {
            return Ok(catalog);
        }
        let older: Vec<Release> = client
            .get(format!("{api}/releases?per_page=100&page={page}"))
            .timeout(Duration::from_secs(12))
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
    cancel: &AtomicBool,
    progress: impl Fn(u64, u64) -> Result<()> + Sync,
) -> Result<Vec<u8>> {
    download_verified_from_repo(client, asset, "horizon-rust/horizon", cancel, progress)
}

pub(crate) fn download_verified_from_repo(
    client: &Client,
    asset: &Asset,
    repository: &str,
    cancel: &AtomicBool,
    progress: impl Fn(u64, u64) -> Result<()> + Sync,
) -> Result<Vec<u8>> {
    let url = download_url(&asset.browser_download_url, repository)?;
    let fallback = api_asset_url(asset, repository)?;
    ensure!(
        asset.size > 0 && asset.size <= MAX_ARCHIVE,
        "Release archive is empty or too large"
    );
    let checksum = asset
        .digest
        .as_deref()
        .context("Release has no SHA-256 digest; download refused")?;
    let sources: Vec<_> = std::iter::once(url).chain(fallback).collect();
    let bytes = download_from_sources(client, &sources, asset.size, checksum, cancel, &progress)?;
    progress(asset.size, asset.size)?;
    Ok(bytes)
}

fn download_from_sources(
    client: &Client,
    sources: &[url::Url],
    size: u64,
    checksum: &str,
    cancel: &AtomicBool,
    progress: &(impl Fn(u64, u64) -> Result<()> + Sync),
) -> Result<Vec<u8>> {
    let mut first_error = None;
    for source in sources {
        network::check_cancel(cancel)?;
        let result: Result<Vec<u8>> = (|| {
            let bytes = if size >= 4 * 1024 * 1024 {
                download_parallel(client, source, size as usize, progress)?
            } else {
                download_stream(client, source, size, progress)?
            };
            verify_digest(&bytes, checksum)?;
            Ok(bytes)
        })();
        match result {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                network::check_cancel(cancel)?;
                first_error = Some(match first_error {
                    Some(previous)
                        if network::is_temporary(&previous) && !network::is_temporary(&error) =>
                    {
                        previous.context(format!("The GitHub asset API fallback failed: {error:#}"))
                    }
                    Some(previous) => {
                        error.context(format!("The GitHub release download failed: {previous:#}"))
                    }
                    None => error,
                });
            }
        }
    }
    Err(first_error.expect("a validated asset has at least one download URL"))
}

fn download_url(text: &str, repository: &str) -> Result<url::Url> {
    let url = url::Url::parse(text)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url
                .path()
                .starts_with(&format!("/{repository}/releases/download/")),
        "Unexpected release download URL"
    );
    Ok(url)
}

/// GitHub's browser download endpoint occasionally returns gateway errors while the
/// release asset API still redirects to the same file. Accept only the exact upstream
/// asset endpoint; the checksum is still verified for every source.
fn api_asset_url(asset: &Asset, repository: &str) -> Result<Option<url::Url>> {
    let Some(text) = asset.url.as_deref() else {
        return Ok(None);
    };
    let url = url::Url::parse(text).context("Invalid GitHub asset API URL")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("api.github.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Unexpected GitHub asset API URL"
    );
    let id = url
        .path()
        .strip_prefix(&format!("/repos/{repository}/releases/assets/"))
        .context("Unexpected GitHub asset API URL")?;
    ensure!(
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()),
        "Unexpected GitHub asset API URL"
    );
    Ok(Some(url))
}

fn download_request(client: &Client, url: &url::Url) -> reqwest::blocking::RequestBuilder {
    let request = client.get(url.clone());
    if url.host_str() == Some("api.github.com") {
        request.header(reqwest::header::ACCEPT, "application/octet-stream")
    } else {
        request
    }
}

fn download_stream(
    client: &Client,
    url: &url::Url,
    size: u64,
    progress: &(impl Fn(u64, u64) -> Result<()> + Sync),
) -> Result<Vec<u8>> {
    let mut response = download_request(client, url).send()?.error_for_status()?;
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
            "Release download exceeded the size limit"
        );
        bytes.extend_from_slice(&buffer[..read]);
        if last.elapsed() >= Duration::from_millis(100) {
            progress(bytes.len() as u64, size)?;
            last = std::time::Instant::now();
        }
    }
    ensure!(
        bytes.len() as u64 == size,
        "Release download was incomplete"
    );
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
                        let mut response = download_request(client, url)
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
                            ensure!(count > 0, "Release download was incomplete");
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
    extract_program(bytes, archive_name, expected, destination)
}

pub(crate) fn extract_program(
    bytes: &[u8],
    archive_name: &str,
    expected: &str,
    destination: &Path,
) -> Result<()> {
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
                    "Release archive contains duplicate executables"
                );
                let mut data = Vec::new();
                entry
                    .by_ref()
                    .take(MAX_ARCHIVE + 1)
                    .read_to_end(&mut data)?;
                ensure!(
                    data.len() as u64 <= MAX_ARCHIVE,
                    "Extracted executable is too large"
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
                    "Release archive contains duplicate executables"
                );
                let mut data = Vec::new();
                entry
                    .by_ref()
                    .take(MAX_ARCHIVE + 1)
                    .read_to_end(&mut data)?;
                ensure!(
                    data.len() as u64 <= MAX_ARCHIVE,
                    "Extracted executable is too large"
                );
                program = Some(data);
            }
        }
    } else {
        bail!("Unsupported release archive format");
    }
    let bytes =
        program.with_context(|| format!("The release archive does not contain {expected}"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(
        destination.parent().context("Missing destination folder")?,
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::TcpListener,
        sync::{Arc, atomic::Ordering},
        thread,
    };

    fn server(replies: Vec<(u16, String)>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                assert!(
                    String::from_utf8(request)
                        .unwrap()
                        .starts_with("GET /releases?per_page=100 ")
                );
                write!(stream, "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (address, task)
    }

    fn download_server(replies: Vec<(u16, Vec<u8>)>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                write!(
                    stream,
                    "HTTP/1.1 {status} Response\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (address, task)
    }

    fn releases() -> String {
        let (engine, game) = platform_assets(std::env::consts::OS, std::env::consts::ARCH).unwrap();
        serde_json::json!([
            {"tag_name":"launcher@0.2.38","name":"Launcher","published_at":"2026-11-04","prerelease":false,"draft":false,
             "assets":[{"name":engine,"browser_download_url":"https://github.com/horizon-rust/horizon/releases/download/test/engine","size":4,"digest":null}]},
            {"tag_name":"nightly","name":"Nightly 2026-11-04 · abc123","published_at":"2026-11-04","prerelease":true,"draft":false,
             "assets":[{"name":game,"browser_download_url":"https://github.com/horizon-rust/horizon/releases/download/test/game","size":4,"digest":null}]}
        ]).to_string()
    }

    #[test]
    fn update_check_recovers_from_a_temporary_http_failure() {
        let (url, server) = server(vec![(503, "{}".into()), (200, releases())]);
        let retries = std::sync::Mutex::new(Vec::new());
        let catalog = fetch_catalog_at(
            &Client::builder().no_proxy().build().unwrap(),
            Channel::Nightly,
            &url,
            &AtomicBool::new(false),
            &[Duration::ZERO; 2],
            |attempt, _| retries.lock().unwrap().push(attempt),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(catalog.game_version().unwrap(), "nightly-2026-11-04-abc123");
        assert_eq!(*retries.lock().unwrap(), [2]);
    }

    #[test]
    fn invalid_responses_and_rate_limits_are_not_retried() {
        for reply in [(403, "{}".into()), (200, "invalid JSON".into())] {
            let (url, server) = server(vec![reply]);
            let result = fetch_catalog_at(
                &Client::builder().no_proxy().build().unwrap(),
                Channel::Nightly,
                &url,
                &AtomicBool::new(false),
                &[Duration::ZERO; 2],
                |_, _| panic!("a permanent response must not be retried"),
            );
            assert!(result.is_err());
            server.join().unwrap();
        }
    }

    #[test]
    fn cancellation_prevents_another_update_request_after_failure() {
        let (url, server) = server(vec![(503, "{}".into())]);
        let cancel = Arc::new(AtomicBool::new(false));
        let result = fetch_catalog_at(
            &Client::builder().no_proxy().build().unwrap(),
            Channel::Nightly,
            &url,
            &cancel,
            &[Duration::from_secs(30)],
            |_, _| cancel.store(true, Ordering::Relaxed),
        );
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        server.join().unwrap();
    }

    #[test]
    fn engine_download_falls_back_to_the_release_asset_api() {
        let (address, server) = download_server(vec![(504, Vec::new()), (200, b"test".to_vec())]);
        let client = Client::builder().no_proxy().build().unwrap();
        let sources = [
            url::Url::parse(&format!("{address}/release")).unwrap(),
            url::Url::parse(&format!("{address}/api")).unwrap(),
        ];
        let checksum = format!("sha256:{}", digest_hex(b"test"));
        let bytes = download_from_sources(
            &client,
            &sources,
            4,
            &checksum,
            &AtomicBool::new(false),
            &|_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(bytes, b"test");
        server.join().unwrap();
    }

    #[test]
    fn launcher_and_engine_downloads_cannot_substitute_each_others_repositories() {
        for repository in ["horizon-rust/horizon", "marktible/mcrslauncher"] {
            let url = format!("https://github.com/{repository}/releases/download/v1.2.3/package");
            assert!(download_url(&url, repository).is_ok());
            let other = if repository == "marktible/mcrslauncher" {
                "horizon-rust/horizon"
            } else {
                "marktible/mcrslauncher"
            };
            assert!(download_url(&url, other).is_err());
            assert!(
                download_url(
                    &format!(
                        "https://github.com/{repository}-fake/releases/download/v1.2.3/package"
                    ),
                    repository
                )
                .is_err()
            );
        }
    }

    #[test]
    fn api_fallback_only_accepts_the_upstream_release_asset_endpoint() {
        let asset = |url: &str| Asset {
            name: "horizon-launcher-linux-x86_64.tar.gz".into(),
            browser_download_url:
                "https://github.com/horizon-rust/horizon/releases/download/test/engine".into(),
            url: Some(url.into()),
            size: 4,
            digest: Some(format!("sha256:{}", digest_hex(b"test"))),
            updated_at: String::new(),
        };
        assert!(
            api_asset_url(
                &asset("https://api.github.com/repos/horizon-rust/horizon/releases/assets/123"),
                "horizon-rust/horizon"
            )
            .unwrap()
            .is_some()
        );
        for invalid in [
            "https://example.test/repos/horizon-rust/horizon/releases/assets/123",
            "https://api.github.com/repos/other/horizon/releases/assets/123",
            "https://api.github.com/repos/horizon-rust/horizon/releases/assets/not-a-number",
            "https://api.github.com/repos/horizon-rust/horizon/releases/assets/123?x=1",
            "http://api.github.com/repos/horizon-rust/horizon/releases/assets/123",
        ] {
            assert!(
                api_asset_url(&asset(invalid), "horizon-rust/horizon").is_err(),
                "{invalid}"
            );
        }
    }
}
