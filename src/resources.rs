//! Materialize the version pinned by Horizon from Mojang's public distribution files.
//! No account, session token, or other launcher's installation is consulted.
use crate::{
    engine::{Output, Reporter},
    network,
    progress::TransferProgress,
};
use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_FILE: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
struct File {
    url: String,
    sha1: String,
    size: Option<u64>,
    path: PathBuf,
}

fn hash(text: &str) -> Result<&str> {
    ensure!(
        text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid Mojang SHA-1 checksum"
    );
    Ok(text)
}

fn relative(text: &str) -> Result<&Path> {
    ensure!(
        !text.is_empty()
            && !text.contains(['\\', ':'])
            && text
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "Unsafe resource path"
    );
    Ok(Path::new(text))
}

fn url(text: &str) -> Result<String> {
    let parsed = url::Url::parse(text)?;
    ensure!(
        parsed.scheme() == "https"
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.port().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && matches!(
                parsed.host_str(),
                Some(
                    "piston-meta.mojang.com"
                        | "piston-data.mojang.com"
                        | "launchermeta.mojang.com"
                        | "launcher.mojang.com"
                        | "libraries.minecraft.net"
                        | "resources.download.minecraft.net"
                )
            ),
        "Unexpected Minecraft resource download host"
    );
    Ok(parsed.to_string())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Minecraft metadata is missing {key}"))
}

fn file(value: &Value, path: PathBuf) -> Result<File> {
    let size = value["size"]
        .as_u64()
        .context("Minecraft metadata is missing the download size")?;
    ensure!(
        size > 0 && size <= MAX_FILE,
        "Resource file is empty or too large"
    );
    Ok(File {
        url: url(text(value, "url")?)?,
        sha1: hash(text(value, "sha1")?)?.to_owned(),
        size: Some(size),
        path,
    })
}

fn verified(path: &Path, checksum: &str, size: Option<u64>) -> Result<bool> {
    let Ok(mut input) = fs::File::open(path) else {
        return Ok(false);
    };
    let length = input.metadata()?.len();
    if length > MAX_FILE || size.is_some_and(|size| length != size) {
        return Ok(false);
    }
    let mut digest = Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()).eq_ignore_ascii_case(checksum))
}

fn download(
    client: &Client,
    file: &File,
    cancel: &AtomicBool,
    progress: &(dyn Fn(u64) + Sync),
) -> Result<()> {
    network::check_cancel(cancel)?;
    if verified(&file.path, &file.sha1, file.size)? {
        return Ok(());
    }
    network::retry(
        cancel,
        &[Duration::from_secs(1), Duration::from_secs(2)],
        |_, _| {},
        || {
            fs::create_dir_all(file.path.parent().context("Missing resource directory")?)?;
            let mut temporary = tempfile::NamedTempFile::new_in(file.path.parent().unwrap())?;
            let mut response = client
                .get(&file.url)
                .timeout(Duration::from_secs(120))
                .send()?
                .error_for_status()?;
            let mut digest = Sha1::new();
            let mut bytes = 0;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                network::check_cancel(cancel)?;
                let count = response.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                bytes += count as u64;
                ensure!(
                    bytes <= file.size.unwrap_or(8 * 1024 * 1024).min(MAX_FILE),
                    "Resource download exceeded its expected size"
                );
                digest.update(&buffer[..count]);
                temporary.write_all(&buffer[..count])?;
                progress(bytes);
            }
            ensure!(
                file.size.is_none_or(|size| bytes == size),
                "Resource download was incomplete"
            );
            ensure!(
                format!("{:x}", digest.finalize()).eq_ignore_ascii_case(&file.sha1),
                "Minecraft resource checksum mismatch; download rejected"
            );
            network::check_cancel(cancel)?;
            temporary.as_file().sync_all()?;
            temporary.persist(&file.path).map_err(|e| e.error)?;
            Ok(())
        },
    )
}

fn library_allowed(library: &Value, os: &str) -> bool {
    library["rules"].as_array().is_none_or(|rules| {
        rules
            .iter()
            .rfind(|rule| rule["os"]["name"].as_str().is_none_or(|name| name == os))
            .is_some_and(|rule| rule["action"] == "allow")
    })
}

fn files(
    manifest: &Value,
    pins: &Value,
    root: &Path,
    home: &Path,
    version: &str,
    os: &str,
) -> Result<(Vec<File>, File)> {
    ensure!(
        manifest["id"] == version,
        "Resource version does not match the installed game"
    );
    let client = file(
        &manifest["downloads"]["client"],
        root.join("versions")
            .join(version)
            .join(format!("{version}.jar")),
    )?;
    ensure!(
        pins["client_sha1"]
            .as_str()
            .is_some_and(|pin| pin.eq_ignore_ascii_case(&client.sha1)),
        "Minecraft client does not match Horizon's version pin"
    );
    let server = file(
        &manifest["downloads"]["server"],
        home.join("server-jars").join(format!("{version}.jar")),
    )?;
    ensure!(
        pins["server_sha1"]
            .as_str()
            .is_some_and(|pin| pin.eq_ignore_ascii_case(&server.sha1)),
        "Minecraft server does not match Horizon's version pin"
    );
    let mut files = vec![client, server];
    for library in manifest["libraries"]
        .as_array()
        .context("Missing Minecraft libraries")?
        .iter()
        .filter(|library| library_allowed(library, os))
    {
        let artifact = &library["downloads"]["artifact"];
        if !artifact.is_null() {
            files.push(file(
                artifact,
                root.join("libraries")
                    .join(relative(text(artifact, "path")?)?),
            )?);
        }
    }
    let index = &manifest["assetIndex"];
    let id = relative(text(index, "id")?)?;
    ensure!(id.components().count() == 1, "Unsafe asset index name");
    let index = file(
        index,
        root.join("assets/indexes")
            .join(format!("{}.json", id.display())),
    )?;
    Ok((files, index))
}

fn assets(index: &Value, root: &Path) -> Result<Vec<File>> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for object in index["objects"]
        .as_object()
        .context("Missing Minecraft assets")?
        .values()
    {
        let checksum = hash(text(object, "hash")?)?;
        if !seen.insert(checksum) {
            continue;
        }
        let size = object["size"].as_u64().context("Missing asset size")?;
        ensure!(size <= MAX_FILE, "Resource file is too large");
        let suffix = format!("{}/{checksum}", &checksum[..2]);
        files.push(File {
            url: format!("https://resources.download.minecraft.net/{suffix}"),
            sha1: checksum.to_owned(),
            size: Some(size),
            path: root.join("assets/objects").join(suffix),
        });
    }
    Ok(files)
}

fn group(
    client: &Client,
    files: &[File],
    cancel: &AtomicBool,
    stage: &str,
    report: Reporter<'_>,
) -> Result<()> {
    report(Output::Stage(stage.into()));
    let total: u64 = files.iter().filter_map(|file| file.size).sum();
    ensure!(total <= 8 * 1024 * 1024 * 1024, "Resource set is too large");
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let status = Mutex::new((vec![0u64; files.len()], Instant::now()));
    let started = Instant::now();
    std::thread::scope(|scope| -> Result<()> {
        let workers: Vec<_> = (0..8.min(files.len()))
            .map(|_| {
                scope.spawn(|| -> Result<()> {
                    while !failed.load(Ordering::Relaxed) {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = files.get(index) else {
                            break;
                        };
                        let progress = |bytes| {
                            let mut state = status.lock().unwrap_or_else(|e| e.into_inner());
                            state.0[index] = bytes;
                            if state.1.elapsed() >= Duration::from_millis(150) {
                                let downloaded = state.0.iter().sum();
                                report(Output::Progress(TransferProgress {
                                    downloaded,
                                    total,
                                    bytes_per_second: downloaded as f64
                                        / started.elapsed().as_secs_f64().max(0.001),
                                }));
                                state.1 = Instant::now();
                            }
                        };
                        if let Err(error) = download(client, file, cancel, &progress) {
                            failed.store(true, Ordering::Relaxed);
                            return Err(error);
                        }
                        progress(file.size.unwrap_or(0));
                    }
                    Ok(())
                })
            })
            .collect();
        let mut failure = None;
        for worker in workers {
            let result = worker
                .join()
                .map_err(|_| anyhow::anyhow!("Resource download worker stopped"))?;
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    })?;
    network::check_cancel(cancel)?;
    report(Output::Progress(TransferProgress {
        downloaded: total,
        total,
        bytes_per_second: 0.,
    }));
    Ok(())
}

pub fn prepare(
    client: &Client,
    home: &Path,
    game: &Path,
    cancel: &AtomicBool,
    report: Reporter<'_>,
) -> Result<PathBuf> {
    network::check_cancel(cancel)?;
    let lock: Value = serde_json::from_slice(&fs::read(game.join("research-lock.json"))?)?;
    let pins = &lock["minecraft"];
    let version = text(pins, "version")?;
    ensure!(
        !version.is_empty()
            && version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
        "Unsafe Minecraft version"
    );
    ensure!(
        relative(version)?.components().count() == 1,
        "Unsafe Minecraft version"
    );
    let checksum = hash(text(pins, "manifest_sha1")?)?;
    let root = home.join("minecraft");
    let manifest = File {
        url: format!("https://piston-meta.mojang.com/v1/packages/{checksum}/{version}.json"),
        sha1: checksum.into(),
        size: None,
        path: root
            .join("versions")
            .join(version)
            .join(format!("{version}.json")),
    };
    report(Output::Stage(format!(
        "Getting Minecraft {version} resource metadata"
    )));
    download(client, &manifest, cancel, &|_| {})?;
    let manifest: Value = serde_json::from_slice(&fs::read(manifest.path)?)?;
    let os = if cfg!(target_os = "macos") {
        "osx"
    } else {
        std::env::consts::OS
    };
    let (files, index) = files(&manifest, pins, &root, home, version, os)?;
    group(
        client,
        &files,
        cancel,
        "Downloading the game files and libraries",
        report,
    )?;
    download(client, &index, cancel, &|_| {})?;
    let index: Value = serde_json::from_slice(&fs::read(index.path)?)?;
    group(
        client,
        &assets(&index, &root)?,
        cancel,
        "Downloading sounds, fonts, and languages",
        report,
    )?;
    Ok(root)
}

#[cfg(test)]
pub(crate) fn fixture(home: &Path, game: &Path) {
    let checksum = |bytes: &[u8]| format!("{:x}", Sha1::digest(bytes));
    let index = br#"{"objects":{}}"#;
    let manifest = serde_json::to_vec(&serde_json::json!({"id":"26.3","libraries":[],
        "downloads":{"client":{"url":"https://piston-data.mojang.com/client","sha1":checksum(b"client"),"size":6},
        "server":{"url":"https://piston-data.mojang.com/server","sha1":checksum(b"server"),"size":6}},
        "assetIndex":{"id":"test","url":"https://piston-meta.mojang.com/index","sha1":checksum(index),"size":index.len()}})).unwrap();
    for (path, bytes) in [
        (
            home.join("minecraft/versions/26.3/26.3.jar"),
            b"client".as_slice(),
        ),
        (home.join("server-jars/26.3.jar"), b"server".as_slice()),
        (
            home.join("minecraft/versions/26.3/26.3.json"),
            manifest.as_slice(),
        ),
        (
            home.join("minecraft/assets/indexes/test.json"),
            index.as_slice(),
        ),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fs::create_dir_all(game).unwrap();
    fs::write(game.join("research-lock.json"), serde_json::json!({"minecraft":{"version":"26.3","manifest_sha1":checksum(&manifest),"client_sha1":checksum(b"client"),"server_sha1":checksum(b"server")}}).to_string()).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::BufRead, net::TcpListener};

    #[test]
    fn validates_official_urls_paths_pins_and_deduplicates_asset_downloads() {
        for invalid in [
            "http://piston-data.mojang.com/client",
            "https://example.com/client",
            "https://piston-data.mojang.com@evil.test/client",
        ] {
            assert!(url(invalid).is_err());
        }
        for invalid in [
            "../file",
            "/file",
            "a/../../file",
            "a\\file",
            "C:/file",
            "",
            "a//file",
        ] {
            assert!(relative(invalid).is_err());
        }
        assert!(relative("org/library/client.jar").is_ok());
        let hash = "a".repeat(40);
        let index = serde_json::json!({"objects":{"one":{"hash":hash,"size":12},"two":{"hash":hash,"size":12}}});
        let objects = assets(&index, Path::new("cache")).unwrap();
        assert_eq!(objects.len(), 1);
        assert!(
            objects[0]
                .path
                .ends_with(format!("assets/objects/aa/{hash}"))
        );
        assert!(
            assets(
                &serde_json::json!({"objects":{"bad":{"hash":"../bad","size":1}}}),
                Path::new("cache")
            )
            .is_err()
        );
    }

    #[test]
    fn cached_files_are_verified_reused_and_repaired_without_partial_replacement() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for body in ["good", "good", "evil"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = std::io::BufReader::new(&mut stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    request.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n{body}"
                )
                .unwrap();
            }
        });
        let temporary = tempfile::tempdir().unwrap();
        let file = File {
            url: format!("http://{address}/file"),
            sha1: format!("{:x}", Sha1::digest(b"good")),
            size: Some(4),
            path: temporary.path().join("file"),
        };
        let client = Client::builder().no_proxy().build().unwrap();
        let cancel = AtomicBool::new(false);
        download(&client, &file, &cancel, &|_| {}).unwrap();
        download(&client, &file, &cancel, &|_| {
            panic!("A valid cached file must not download again")
        })
        .unwrap();
        fs::write(&file.path, "old").unwrap();
        download(&client, &file, &cancel, &|_| {}).unwrap();
        assert_eq!(fs::read(&file.path).unwrap(), b"good");
        fs::write(&file.path, "old").unwrap();
        assert!(
            download(&client, &file, &cancel, &|_| {})
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
        assert_eq!(fs::read(&file.path).unwrap(), b"old");
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 1);
        assert!(
            download(&client, &file, &AtomicBool::new(true), &|_| {})
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        server.join().unwrap();
    }

    #[test]
    fn prepares_its_own_cache_without_an_account_or_an_external_minecraft_folder() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("launcher");
        let game = home.join("versions/test");
        fixture(&home, &game);
        let client = Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
            .build()
            .unwrap();
        let root = prepare(&client, &home, &game, &AtomicBool::new(false), &|_| {}).unwrap();
        assert_eq!(root, home.join("minecraft"));
        let manifest = root.join("versions/26.3/26.3.json");
        let mut data: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        data["downloads"]["client"]["sha1"] = Value::String("b".repeat(40));
        let pins: Value =
            serde_json::from_slice(&fs::read(game.join("research-lock.json")).unwrap()).unwrap();
        assert!(files(&data, &pins["minecraft"], &root, &home, "26.3", "linux").is_err());
    }
}
