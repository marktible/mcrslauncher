//! Public skin images are cached separately from credentials. A skin upload consumes an
//! upstream read-once session file; its token stays in this worker and goes only to Mojang.
use crate::{
    engine::Engine,
    model::{self, Account, Paths},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

const MAX_SKIN: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkinModel {
    #[default]
    Classic,
    Slim,
}

impl SkinModel {
    pub fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Slim => "slim",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SkinFile {
    pub name: String,
    pub png: Vec<u8>,
    pub head: Vec<u8>,
}

pub fn normalize_uuid(uuid: &str) -> Result<String> {
    let uuid: String = uuid.chars().filter(|c| *c != '-').collect();
    ensure!(
        uuid.len() == 32 && uuid.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid Minecraft account UUID"
    );
    Ok(uuid.to_ascii_lowercase())
}

pub fn skin_head(bytes: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        bytes.len() as u64 <= MAX_SKIN,
        "Skin PNG must be smaller than 1 MiB"
    );
    let reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let dimensions = reader
        .into_dimensions()
        .context("Select a PNG skin image")?;
    ensure!(
        matches!(dimensions, (64, 32 | 64)),
        "Minecraft skins must be 64×64 or 64×32 pixels"
    );
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?.to_rgba8();
    let mut head = Vec::with_capacity(8 * 8 * 4);
    for y in 0..8 {
        for x in 0..8 {
            let face = image.get_pixel(x + 8, y + 8).0;
            let hat = image.get_pixel(x + 40, y + 8).0;
            let alpha = u32::from(hat[3]);
            for channel in 0..3 {
                head.push(
                    ((u32::from(hat[channel]) * alpha
                        + u32::from(face[channel]) * (255 - alpha)
                        + 127)
                        / 255) as u8,
                );
            }
            head.push(255);
        }
    }
    Ok(head)
}

pub fn read_skin(path: &Path) -> Result<SkinFile> {
    let mut png = Vec::new();
    fs::File::open(path)?
        .take(MAX_SKIN + 1)
        .read_to_end(&mut png)?;
    let head = skin_head(&png)?;
    Ok(SkinFile {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        png,
        head,
    })
}

pub fn texture_url(text: &str) -> Result<url::Url> {
    let mut url = url::Url::parse(text)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str() == Some("textures.minecraft.net")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Unexpected Minecraft skin texture URL"
    );
    let texture = url
        .path()
        .strip_prefix("/texture/")
        .context("Invalid Minecraft texture path")?;
    ensure!(
        !texture.is_empty() && texture.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid Minecraft texture identifier"
    );
    url.set_scheme("https")
        .map_err(|_| anyhow::anyhow!("Invalid skin URL"))?;
    Ok(url)
}

#[derive(Serialize, Deserialize)]
struct CacheKey {
    skin: Option<(String, String)>,
}

fn cache_paths(paths: &Paths, uuid: &str) -> Result<(PathBuf, PathBuf)> {
    let uuid = normalize_uuid(uuid)?;
    let directory = paths.root.join("profiles");
    fs::create_dir_all(&directory)?;
    Ok((
        directory.join(format!("{uuid}.png")),
        directory.join(format!("{uuid}.json")),
    ))
}

fn cache_skin(paths: &Paths, account: &Account, png: &[u8]) -> Result<()> {
    let (image, metadata) = cache_paths(paths, &account.uuid)?;
    model::atomic_write(&image, png)?;
    model::atomic_write(
        &metadata,
        &serde_json::to_vec(&CacheKey {
            skin: account.skin.clone(),
        })?,
    )
}

fn public_texture(client: &reqwest::blocking::Client, uuid: &str) -> Result<String> {
    let uuid = normalize_uuid(uuid)?;
    let response = client
        .get(format!(
            "https://sessionserver.mojang.com/session/minecraft/profile/{uuid}"
        ))
        .timeout(Duration::from_secs(10))
        .send()?
        .error_for_status()?;
    let mut body = Vec::new();
    response.take(64 * 1024 + 1).read_to_end(&mut body)?;
    ensure!(
        body.len() <= 64 * 1024,
        "Minecraft profile response was too large"
    );
    let profile: serde_json::Value = serde_json::from_slice(&body)?;
    let value = profile["properties"]
        .as_array()
        .and_then(|p| p.iter().find(|p| p["name"] == "textures"))
        .and_then(|p| p["value"].as_str())
        .context("No public skin texture for this profile")?;
    let textures: serde_json::Value = serde_json::from_slice(&STANDARD.decode(value)?)?;
    Ok(textures["textures"]["SKIN"]["url"]
        .as_str()
        .context("This profile has no skin texture")?
        .to_string())
}

fn load_head(
    client: &reqwest::blocking::Client,
    paths: &Paths,
    account: &Account,
) -> Result<Vec<u8>> {
    let (cached, metadata) = cache_paths(paths, &account.uuid)?;
    if let (Ok(png), Ok(key)) = (fs::read(&cached), fs::read(&metadata))
        && serde_json::from_slice::<CacheKey>(&key).is_ok_and(|key| key.skin == account.skin)
        && let Ok(head) = skin_head(&png)
    {
        return Ok(head);
    }
    let source = match &account.skin {
        Some((url, _)) => url.clone(),
        None => public_texture(client, &account.uuid)?,
    };
    let response = client
        .get(texture_url(&source)?)
        .timeout(Duration::from_secs(10))
        .send()?
        .error_for_status()?;
    let mut png = Vec::new();
    response.take(MAX_SKIN + 1).read_to_end(&mut png)?;
    let head = skin_head(&png)?;
    cache_skin(paths, account, &png)?;
    Ok(head)
}

/// Consume only a session file inside this launcher's own sessions folder; delete it before
/// parsing, including malformed files. No access or refresh token enters a UI event or log.
pub fn consume_session(paths: &Paths, file: &Path, uuid: &str) -> Result<Zeroizing<String>> {
    let directory = paths.game_home().join("sessions").canonicalize()?;
    ensure!(
        !fs::symlink_metadata(file)?.file_type().is_symlink(),
        "Refusing a linked Minecraft session file"
    );
    let file = file.canonicalize()?;
    ensure!(
        file.parent() == Some(directory.as_path()),
        "Minecraft session file is outside the private sessions folder"
    );
    let mut bytes = Zeroizing::new(Vec::new());
    let result = fs::File::open(&file).and_then(|f| f.take(64 * 1024 + 1).read_to_end(&mut bytes));
    fs::remove_file(&file).context("Could not remove the read-once Minecraft session")?;
    result.context("Could not read the Minecraft session")?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "Minecraft session response was too large"
    );
    #[derive(Deserialize)]
    struct Session {
        uuid: String,
        access_token: String,
    }
    let session: Session = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Invalid Minecraft session response"))?;
    let token = Zeroizing::new(session.access_token);
    ensure!(
        normalize_uuid(&session.uuid)? == normalize_uuid(uuid)?,
        "Minecraft session belongs to another account"
    );
    ensure!(!token.is_empty(), "Minecraft session has no access token");
    Ok(token)
}

fn upload_skin(
    engine: &Engine,
    account: &Account,
    skin: &SkinFile,
    model: SkinModel,
) -> Result<()> {
    skin_head(&skin.png)?;
    normalize_uuid(&account.uuid)?;
    engine.verify_local_engine()?;
    let session = engine.json(&["account", "session", &account.uuid])?;
    let file = session["session_file"]
        .as_str()
        .map(PathBuf::from)
        .context("Minecraft sign-in needs renewal. Sign in again before changing your skin.")?;
    let token = consume_session(&engine.paths, &file, &account.uuid)?;
    let form = reqwest::blocking::multipart::Form::new()
        .text("variant", model.name())
        .part(
            "file",
            reqwest::blocking::multipart::Part::bytes(skin.png.clone())
                .file_name("skin.png")
                .mime_str("image/png")?,
        );
    let response = engine
        .client
        .post("https://api.minecraftservices.com/minecraft/profile/skins")
        .bearer_auth(token.as_str())
        .multipart(form)
        .timeout(Duration::from_secs(45))
        .send()
        .context("Could not reach Minecraft's skin service")?;
    drop(token);
    match response.status().as_u16() {
        200..=299 => {}
        401 | 403 => bail!("Minecraft refused the skin change. Sign in again and retry."),
        429 => bail!("Minecraft is limiting skin changes. Wait a moment and retry."),
        code => bail!("Minecraft could not apply that skin (HTTP {code})."),
    }
    // Keep the chosen skin immediately; the session service can cache the old texture briefly.
    let _ = cache_skin(&engine.paths, account, &skin.png);
    // Upstream refresh records the new public skin URL, while its keychain continues owning auth.
    let _ = engine.run(&["account", "refresh", &account.uuid], &|_| {}, true);
    Ok(())
}

pub enum ProfileCommand {
    Load(Vec<Account>),
    Choose {
        uuid: String,
    },
    Upload {
        account: Account,
        model: SkinModel,
        skin: SkinFile,
    },
}

pub enum ProfileEvent {
    Head {
        uuid: String,
        rgba: Vec<u8>,
    },
    Chosen {
        uuid: String,
        skin: Option<SkinFile>,
    },
    Uploaded {
        uuid: String,
    },
    Failed {
        message: String,
    },
}

pub struct ProfileWorker {
    commands: SyncSender<ProfileCommand>,
    pub events: Receiver<ProfileEvent>,
}

impl ProfileWorker {
    pub fn new(paths: Paths, context: eframe::egui::Context) -> Self {
        let (commands, inbox) = mpsc::sync_channel(4);
        let (sender, events) = mpsc::sync_channel(8);
        thread::spawn(move || {
            let send = |event| {
                if sender.send(event).is_ok() {
                    context.request_repaint();
                }
            };
            let mut loaded = HashMap::<String, Option<(String, String)>>::new();
            let engine = match Engine::new(paths, Arc::new(AtomicBool::new(false))) {
                Ok(engine) => engine,
                Err(error) => {
                    send(ProfileEvent::Failed {
                        message: error.to_string(),
                    });
                    return;
                }
            };
            while let Ok(command) = inbox.recv() {
                match command {
                    ProfileCommand::Load(accounts) => {
                        for account in accounts {
                            if loaded.get(&account.uuid) == Some(&account.skin) {
                                continue;
                            }
                            if let Ok(rgba) = load_head(&engine.client, &engine.paths, &account) {
                                loaded.insert(account.uuid.clone(), account.skin);
                                send(ProfileEvent::Head {
                                    uuid: account.uuid,
                                    rgba,
                                });
                            }
                        }
                    }
                    ProfileCommand::Choose { uuid } => {
                        let result = rfd::FileDialog::new()
                            .set_title("Choose Minecraft skin PNG")
                            .add_filter("Minecraft skin", &["png"])
                            .pick_file()
                            .map(|path| read_skin(&path))
                            .transpose();
                        match result {
                            Ok(skin) => send(ProfileEvent::Chosen { uuid, skin }),
                            Err(error) => send(ProfileEvent::Failed {
                                message: error.to_string(),
                            }),
                        }
                    }
                    ProfileCommand::Upload {
                        account,
                        model,
                        skin,
                    } => match upload_skin(&engine, &account, &skin, model) {
                        Ok(()) => {
                            loaded.insert(account.uuid.clone(), account.skin);
                            send(ProfileEvent::Head {
                                uuid: account.uuid.clone(),
                                rgba: skin.head,
                            });
                            send(ProfileEvent::Uploaded { uuid: account.uuid });
                        }
                        Err(error) => send(ProfileEvent::Failed {
                            message: error.to_string(),
                        }),
                    },
                }
            }
        });
        Self { commands, events }
    }

    pub fn send(&self, command: ProfileCommand) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|_| anyhow::anyhow!("The skin service is busy. Try again in a moment."))
    }
}
