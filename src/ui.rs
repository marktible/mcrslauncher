use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, Stroke, Vec2,
};
use mcrslauncher::{
    engine::{self, Output},
    github::{self, Catalog},
    model::{Account, BuildProfile, Channel, Paths, Receipt, Settings, Snapshot},
    profiles::{ProfileCommand, ProfileEvent, ProfileWorker, SkinFile, SkinModel},
    progress::{TransferProgress, format_duration},
    worker::{Action, Event, Worker},
};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant, SystemTime},
};

const BG: Color32 = Color32::from_rgb(19, 22, 23);
const PANEL: Color32 = Color32::from_rgb(26, 30, 31);
const CARD: Color32 = Color32::from_rgb(30, 35, 35);
const LINE: Color32 = Color32::from_rgb(48, 55, 54);
const TEXT: Color32 = Color32::from_rgb(235, 236, 225);
const MUTED: Color32 = Color32::from_rgb(146, 159, 152);
const ACCENT: Color32 = Color32::from_rgb(231, 151, 92);
const GREEN: Color32 = Color32::from_rgb(151, 191, 142);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Play,
    Mods,
    Updates,
    Settings,
}

enum Confirm {
    UpdateMods,
    Remove(String),
}

struct SkinEditor {
    account: Account,
    model: SkinModel,
    skin: Option<SkinFile>,
    preview: Option<egui::TextureHandle>,
    choosing: bool,
    uploading: bool,
}

pub struct Launcher {
    worker: Worker,
    profiles: ProfileWorker,
    heads: HashMap<String, egui::TextureHandle>,
    skin_editor: Option<SkinEditor>,
    paths: Paths,
    settings: Settings,
    snapshot: Snapshot,
    receipt: Receipt,
    catalog: Option<Catalog>,
    catalog_channel: Channel,
    page: Page,
    busy: bool,
    stage: String,
    progress: Option<TransferProgress>,
    job_started: Option<Instant>,
    last_activity: String,
    error: Option<String>,
    notice: String,
    login: Option<(String, String)>,
    mod_url: String,
    trust_mod: bool,
    confirm: Option<Confirm>,
    logs: VecDeque<String>,
    show_logs: bool,
    pending_check: bool,
    checked_at: Option<SystemTime>,
    last_check: Instant,
    playing: bool,
}

impl Launcher {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_page: &str) -> Self {
        configure(&cc.egui_ctx);
        let paths = Paths::discover().unwrap_or_else(|_| Paths {
            root: std::env::temp_dir().join("mcrslauncher"),
        });
        let (settings, error) = match paths.load_settings() {
            Ok(settings) => (settings, None),
            Err(error) => (
                Settings::default(),
                Some(format!("Settings could not be loaded: {error:#}")),
            ),
        };
        let pending_check = settings.auto_check;
        let mut app = Self {
            worker: Worker::new(paths.clone(), cc.egui_ctx.clone()),
            profiles: ProfileWorker::new(paths.clone(), cc.egui_ctx.clone()),
            heads: HashMap::new(),
            skin_editor: None,
            paths,
            catalog_channel: settings.channel,
            settings,
            snapshot: Snapshot::default(),
            receipt: Receipt::default(),
            catalog: None,
            page: match initial_page {
                "mods" => Page::Mods,
                "updates" => Page::Updates,
                "settings" => Page::Settings,
                _ => Page::Play,
            },
            busy: false,
            stage: String::new(),
            progress: None,
            job_started: None,
            last_activity: String::new(),
            error: None,
            notice: String::new(),
            login: None,
            mod_url: String::new(),
            trust_mod: false,
            confirm: None,
            logs: VecDeque::with_capacity(500),
            show_logs: false,
            pending_check,
            checked_at: None,
            last_check: Instant::now(),
            playing: false,
        };
        app.start(Action::Refresh);
        app.error = error;
        app
    }

    fn start(&mut self, action: Action) {
        if self.busy {
            return;
        }
        let label = action.label().to_string();
        match self.worker.start(action, self.settings.clone()) {
            Ok(()) => {
                self.busy = true;
                self.stage = label;
                self.progress = None;
                self.job_started = Some(Instant::now());
                self.last_activity.clear();
                self.error = None;
                self.notice.clear();
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        // Bound work per frame even if a compiler floods the output pipe.
        for index in 0..128 {
            let Ok(event) = self.worker.events.try_recv() else {
                break;
            };
            match event {
                Event::Output(Output::Log(line)) => {
                    self.last_activity = line.trim().to_string();
                    if self.logs.len() == 500 {
                        self.logs.pop_front();
                    }
                    self.logs.push_back(line);
                }
                Event::Output(Output::Stage(stage)) => {
                    self.stage = stage;
                    self.progress = None;
                    self.last_activity.clear();
                }
                Event::Output(Output::Progress(progress)) => self.progress = Some(progress),
                Event::Output(Output::Login { url, code }) => {
                    if let Err(error) = open::that(&url) {
                        self.error = Some(format!("Open the sign-in link below: {error}"));
                    }
                    self.login = Some((url, code));
                }
                Event::Output(Output::Playing) => {
                    self.playing = true;
                    if self.settings.minimize_on_play {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                }
                Event::Snapshot(snapshot) => {
                    let mut accounts = snapshot.accounts.clone();
                    accounts.sort_by_key(|account| !account.selected);
                    let _ = self.profiles.send(ProfileCommand::Load(accounts));
                    self.snapshot = snapshot;
                }
                Event::Catalog(catalog) => {
                    self.catalog = Some(*catalog);
                    self.catalog_channel = self.settings.channel;
                    self.checked_at = Some(SystemTime::now());
                    self.last_check = Instant::now();
                }
                Event::Receipt(receipt) => self.receipt = receipt,
                Event::Done(result) => {
                    self.busy = false;
                    self.login = None;
                    self.progress = None;
                    self.job_started = None;
                    if self.playing {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        self.playing = false;
                    }
                    match result {
                        Ok(message) => self.notice = message,
                        Err(error) => {
                            if self.logs.len() >= 500 {
                                self.logs.pop_front();
                            }
                            self.logs.push_back(error.clone());
                            self.error = Some(error);
                        }
                    }
                }
            }
            if index == 127 {
                ctx.request_repaint();
            }
        }
        for _ in 0..16 {
            let Ok(event) = self.profiles.events.try_recv() else {
                break;
            };
            match event {
                ProfileEvent::Head { uuid, rgba } => {
                    let image = egui::ColorImage::from_rgba_unmultiplied([8, 8], &rgba);
                    self.heads.insert(
                        uuid.clone(),
                        ctx.load_texture(
                            format!("profile-{uuid}"),
                            image,
                            egui::TextureOptions::NEAREST,
                        ),
                    );
                }
                ProfileEvent::Chosen { uuid, skin } => {
                    if let Some(editor) = &mut self.skin_editor
                        && editor.account.uuid == uuid
                    {
                        editor.choosing = false;
                        if let Some(skin) = skin {
                            editor.preview = Some(ctx.load_texture(
                                "skin-preview",
                                egui::ColorImage::from_rgba_unmultiplied([8, 8], &skin.head),
                                egui::TextureOptions::NEAREST,
                            ));
                            editor.skin = Some(skin);
                        }
                    }
                }
                ProfileEvent::Uploaded { uuid } => {
                    if self
                        .skin_editor
                        .as_ref()
                        .is_some_and(|e| e.account.uuid == uuid)
                    {
                        self.skin_editor = None;
                    }
                    self.notice = "Minecraft skin updated. New game sessions will use it.".into();
                    if !self.busy {
                        self.start(Action::Refresh);
                    }
                }
                ProfileEvent::Failed { message } => {
                    if let Some(editor) = &mut self.skin_editor {
                        editor.choosing = false;
                        editor.uploading = false;
                    }
                    self.error = Some(message);
                }
            }
        }
        if !self.busy
            && (self.pending_check
                || (self.settings.auto_check
                    && self.last_check.elapsed() >= Duration::from_secs(900)))
        {
            self.pending_check = false;
            self.last_check = Instant::now();
            self.start(Action::CheckUpdates);
        }
        if self.settings.auto_check {
            ctx.request_repaint_after(
                Duration::from_secs(900)
                    .saturating_sub(self.last_check.elapsed())
                    .max(Duration::from_secs(1)),
            );
        }
    }

    fn save(&mut self) {
        if let Err(error) = self.paths.save_settings(&self.settings) {
            self.error = Some(format!("Could not save settings: {error:#}"));
        }
    }

    fn open(&mut self, target: impl AsRef<std::ffi::OsStr>) {
        if let Err(error) = open::that(target) {
            self.error = Some(format!("Could not open: {error}"));
        }
    }

    fn engine_ready(&self) -> bool {
        !self.receipt.executable_sha256.is_empty()
    }
    fn has_update(&self) -> bool {
        self.catalog.as_ref().is_some_and(|c| {
            c.game_version().ok().as_deref() != Some(self.receipt.game_tag.as_str())
                || engine::game_fingerprint(c) != self.receipt.game_digest
                || c.engine.tag_name != self.receipt.tag
                || c.engine_asset.digest.as_deref() != Some(self.receipt.digest.as_str())
        })
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("navigation")
            .exact_width(202.)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(20, 28))
                    .stroke(Stroke::new(1_f32, LINE)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(30.), egui::Sense::hover());
                    cube(ui.painter(), r.center() - egui::vec2(0., 4.), 13., ACCENT);
                    ui.label(RichText::new("MCRS").size(23.).strong().color(TEXT));
                });
                ui.add_space(3.);
                ui.label(RichText::new("THE HORIZON LAUNCHER").size(9.).color(MUTED));
                ui.add_space(39.);
                ui.label(RichText::new("YOUR WORKSPACE").size(10.).color(MUTED));
                ui.add_space(12.);
                for (page, title, sub) in [
                    (Page::Play, "Play", "01"),
                    (Page::Mods, "Mods", "02"),
                    (Page::Updates, "Updates", "03"),
                    (Page::Settings, "Settings", "04"),
                ] {
                    let active = self.page == page;
                    let button = egui::Button::new(
                        RichText::new(format!("{sub}    {title}"))
                            .size(14.)
                            .color(if active { ACCENT } else { MUTED }),
                    )
                    .fill(if active {
                        Color32::from_rgb(48, 42, 35)
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(6.)
                    .min_size(egui::vec2(162., 43.));
                    if ui.add(button).clicked() {
                        self.page = page;
                    }
                    ui.add_space(4.);
                }
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.label(
                        RichText::new(concat!("MCRS v", env!("CARGO_PKG_VERSION")))
                            .size(11.)
                            .color(MUTED),
                    );
                    ui.add_space(8.);
                    if ui
                        .link(RichText::new("Horizon on GitHub >").size(12.).color(MUTED))
                        .clicked()
                    {
                        self.open(github::REPOSITORY);
                    }
                    ui.add_space(20.);
                    ui.separator();
                    ui.add_space(12.);
                    ui.label(
                        RichText::new("Built in Rust.\nMade for your next world.")
                            .size(12.)
                            .color(MUTED),
                    );
                });
            });
    }

    fn footer(&mut self, ctx: &egui::Context) {
        let working = self.busy && !self.playing;
        egui::TopBottomPanel::bottom("status")
            .exact_height(if working { 95. } else { 45. })
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(24, 10))
                    .stroke(Stroke::new(1_f32, LINE)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    dot(ui, if self.busy { ACCENT } else { GREEN });
                    let text = if self.busy {
                        self.stage.as_str()
                    } else if !self.notice.is_empty() {
                        &self.notice
                    } else {
                        "Ready for your next adventure"
                    };
                    ui.add_sized(
                        [ui.available_width() - 190., 18.],
                        egui::Label::new(RichText::new(text).size(11.).color(MUTED)).truncate(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button(if self.show_logs {
                                "Hide activity"
                            } else {
                                "Activity log"
                            })
                            .clicked()
                        {
                            self.show_logs = !self.show_logs;
                        }
                        if self.busy
                            && ui
                                .small_button(if self.playing { "Stop game" } else { "Cancel" })
                                .clicked()
                        {
                            self.worker.cancel();
                        }
                    });
                });
                if working {
                    if let Some(progress) = &self.progress {
                        ui.add(
                            egui::ProgressBar::new(progress.fraction())
                                .fill(ACCENT)
                                .desired_height(12.),
                        );
                    } else {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 9.),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(rect, 4., LINE);
                        let elapsed = self
                            .job_started
                            .map(|t| t.elapsed().as_secs_f32())
                            .unwrap_or_default();
                        let width = rect.width() * 0.22;
                        let x = rect.left()
                            + (rect.width() - width) * (elapsed * 1.5).sin().mul_add(0.5, 0.5);
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(x, rect.top()),
                                egui::vec2(width, rect.height()),
                            ),
                            4.,
                            ACCENT,
                        );
                        // Animate independently of the worker's much slower progress reports.
                        // Use a short animation timer only while the bar is visible.
                        if ui.is_rect_visible(rect)
                            && !ctx.input(|i| i.viewport().minimized.unwrap_or(false))
                        {
                            ctx.request_repaint_after(Duration::from_secs_f64(1. / 60.));
                        }
                    }
                    let elapsed =
                        format_duration(self.job_started.map(|t| t.elapsed()).unwrap_or_default());
                    let detail = self
                        .progress
                        .as_ref()
                        .map(TransferProgress::description)
                        .unwrap_or_else(|| {
                            if self.last_activity.is_empty() {
                                "Working…".into()
                            } else {
                                self.last_activity.clone()
                            }
                        });
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("{detail} · elapsed {elapsed}"))
                                .size(11.)
                                .color(MUTED),
                        )
                        .truncate(),
                    );
                    // Measured progress repaints on worker events; only the elapsed clock
                    // needs a timer when there is no animation to draw.
                    ctx.request_repaint_after(Duration::from_secs(1));
                }
            });
    }

    fn play(&mut self, ui: &mut egui::Ui) {
        landscape(ui);
        ui.add_space(24.);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Your next adventure").size(22.).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Open game folder").clicked() {
                    self.open(self.game_folder());
                }
                badge(ui, "NATIVE RUST", GREEN);
            });
        });
        ui.add_space(15.);
        let width = ui.available_width();
        ui.horizontal_top(|ui| {
            card().show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(((width - 13.) * 0.59 - 38.).max(260.));
                    ui.horizontal(|ui| {
                        badge(ui, self.settings.channel.label(), ACCENT);
                        if self.snapshot.native_build {
                            badge(ui, "MODDED", GREEN);
                        }
                    });
                    ui.add_space(16.);
                    ui.label(RichText::new("Minecraft. Reimagined.").size(22.).strong());
                    ui.add_space(6.);
                    ui.label(
                        RichText::new("The world you know, powered by Horizon's Rust engine.")
                            .size(13.)
                            .color(MUTED),
                    );
                    ui.add_space(18.);
                    ui.separator();
                    ui.add_space(10.);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("INSTALLED VERSION").size(10.).color(MUTED));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(
                                    self.snapshot
                                        .selected_version
                                        .as_deref()
                                        .unwrap_or("Not installed"),
                                )
                                .size(12.),
                            );
                        });
                    });
                    ui.add_space(14.);
                    let (label, action) = if !self.engine_ready() {
                        ("Set up Horizon", Action::Setup)
                    } else if !self.snapshot.owner {
                        ("Sign in to Minecraft", Action::SignIn)
                    } else {
                        (
                            if self.receipt.mods_dirty {
                                "Build & play  >"
                            } else {
                                "Play Horizon  >"
                            },
                            Action::Play,
                        )
                    };
                    if primary(ui, label, !self.busy).clicked() {
                        self.start(action);
                    }
                    ui.add_space(7.);
                    ui.label(
                        RichText::new(if self.settings.update_before_play {
                            "Checks for the latest game build before launch"
                        } else {
                            "Using your locally installed game build"
                        })
                        .size(10.)
                        .color(MUTED),
                    );
                });
            });
            ui.add_space(4.);
            card().show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(((width - 13.) * 0.41 - 38.).max(180.));
                    ui.label(RichText::new("MINECRAFT ACCOUNT").size(10.).color(MUTED));
                    ui.add_space(17.);
                    if let Some(account) = self
                        .snapshot
                        .accounts
                        .iter()
                        .find(|a| a.selected)
                        .or(self.snapshot.accounts.first())
                        .cloned()
                    {
                        ui.horizontal(|ui| {
                            self.profile_avatar(ui, &account, 40.);
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&account.name).size(17.).strong());
                                ui.label(RichText::new("Microsoft account").size(11.).color(GREEN));
                            });
                        });
                        ui.add_space(18.);
                        ui.label(
                            RichText::new(if self.snapshot.owner {
                                "Java Edition ownership verified."
                            } else {
                                "Sign in again to verify game ownership."
                            })
                            .size(12.)
                            .color(MUTED),
                        );
                        if ui.button("Change skin").clicked() {
                            self.edit_skin(account);
                        }
                    } else {
                        ui.label(RichText::new("Make yourself at home.").size(17.).strong());
                        ui.add_space(8.);
                        ui.label(
                            RichText::new(
                                "Connect the Microsoft account that owns Minecraft: Java Edition.",
                            )
                            .size(12.)
                            .color(MUTED),
                        );
                        ui.add_space(14.);
                        if ui
                            .add_enabled(
                                !self.busy,
                                egui::Button::new("Sign in with Microsoft")
                                    .min_size(egui::vec2(180., 36.)),
                            )
                            .clicked()
                        {
                            self.start(Action::SignIn);
                        }
                    }
                    ui.add_space(18.);
                    ui.separator();
                    ui.add_space(9.);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{} native mods", self.snapshot.mods.len()))
                                .size(12.)
                                .color(MUTED),
                        );
                        if ui.link("Manage >").clicked() {
                            self.page = Page::Mods;
                        }
                    });
                });
            });
        });
        ui.add_space(19.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                dot(ui, if self.has_update() { ACCENT } else { GREEN });
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(if self.has_update() {
                            "A new horizon is waiting"
                        } else {
                            "Always looking ahead"
                        })
                        .size(13.)
                        .strong(),
                    );
                    ui.label(
                        RichText::new("Follow the latest Horizon builds and release notes.")
                            .size(11.)
                            .color(MUTED),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("View updates >").clicked() {
                        self.page = Page::Updates;
                    }
                });
            });
        });
        ui.add_space(14.);
        ui.label(
            RichText::new("Unofficial launcher. Not affiliated with Mojang or Microsoft.")
                .size(10.)
                .color(MUTED),
        );
    }

    fn mods(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "A world of your own.",
            "Import native Horizon mods from GitHub, then build them for your game.",
        );
        ui.add_space(20.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("IMPORT FROM GITHUB").size(10.).color(ACCENT));
            ui.add_space(12.);
            ui.add(egui::TextEdit::singleline(&mut self.mod_url).hint_text("https://github.com/owner/horizon-mod").desired_width(f32::INFINITY).margin(egui::vec2(12., 11.)));
            ui.add_space(8.);
            ui.label(RichText::new("Repository, release tag, branch, or commit. Horizon native mods only; Java .jar mods are incompatible.").size(11.).color(MUTED));
            ui.add_space(12.);
            ui.checkbox(&mut self.trust_mod, "I trust this source to run native code on my computer.");
            ui.add_space(10.);
            let valid = github::validate_mod_url(&self.mod_url);
            if !self.mod_url.trim().is_empty() && let Err(error) = &valid {
                ui.label(RichText::new(error.to_string()).size(11.).color(ACCENT));
            }
            if primary(ui, "Import & build mod  >", !self.busy && self.trust_mod && valid.is_ok()).clicked() {
                self.trust_mod = false;
                self.start(Action::ImportMod(self.mod_url.trim().to_string()));
            }
        });
        ui.add_space(24.);
        if self.receipt.mods_dirty {
            ui.label(
                RichText::new("Mod changes need a successful build before play. Rebuild to retry.")
                    .color(ACCENT),
            );
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Installed mods ({})", self.snapshot.mods.len()))
                    .size(18.)
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.busy && !self.snapshot.mods.is_empty(),
                        egui::Button::new("Rebuild"),
                    )
                    .clicked()
                {
                    self.start(Action::BuildMods);
                }
                if ui
                    .add_enabled(
                        !self.busy && !self.snapshot.mods.is_empty(),
                        egui::Button::new("Check updates"),
                    )
                    .clicked()
                {
                    self.show_logs = true;
                    self.start(Action::CheckModUpdates);
                }
                if ui
                    .add_enabled(
                        !self.busy && !self.snapshot.mods.is_empty(),
                        egui::Button::new("Update & build"),
                    )
                    .clicked()
                {
                    self.confirm = Some(Confirm::UpdateMods);
                }
            });
        });
        ui.add_space(12.);
        if self.snapshot.mods.is_empty() {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(22.);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Your mod collection starts here.")
                            .size(17.)
                            .color(TEXT),
                    );
                    ui.add_space(5.);
                    ui.label(
                        RichText::new("Paste a GitHub link above to make this world yours.")
                            .size(12.)
                            .color(MUTED),
                    );
                });
                ui.add_space(22.);
            });
        }
        for item in self.snapshot.mods.clone() {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&item.id).size(16.).strong());
                        ui.label(
                            RichText::new(format!("{} · {}", item.repository, item.version))
                                .size(11.)
                                .color(MUTED),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Remove"))
                            .clicked()
                        {
                            self.confirm = Some(Confirm::Remove(item.id.clone()));
                        }
                        if ui.link("Source >").clicked() {
                            self.open(&item.source_url);
                        }
                    });
                });
            });
            ui.add_space(8.);
        }
        ui.add_space(12.);
        ui.label(RichText::new("Incremental builds reuse compiled dependencies. The first native build can take several minutes and several GB of disk space.").size(11.).color(MUTED));
    }

    fn updates(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Keep your world moving.",
            "The latest engine, game builds, and everything that changed.",
        );
        ui.add_space(22.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Release channel").size(16.).strong());
                let before = self.settings.channel;
                egui::ComboBox::from_id_salt("update_channel").selected_text(self.settings.channel.label()).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.channel, Channel::Nightly, "Nightly · newest builds");
                    ui.selectable_value(&mut self.settings.channel, Channel::Stable, "Stable · published releases");
                });
                if before != self.settings.channel {
                    self.catalog = None;
                    self.save();
                    self.pending_check = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(!self.busy, egui::Button::new("Check now")).clicked() { self.start(Action::CheckUpdates); }
                });
            });
            ui.add_space(10.);
            ui.label(RichText::new("Nightly follows the newest published Horizon build. Minecraft assets match the version required by that build.").size(12.).color(MUTED));
            ui.add_space(18.);
            ui.separator();
            ui.add_space(14.);
            if let Some(catalog) = self.catalog.clone().filter(|_| self.catalog_channel == self.settings.channel) {
                ui.horizontal(|ui| {
                    dot(ui, if self.has_update() { ACCENT } else { GREEN });
                    ui.label(RichText::new(if self.has_update() { "Update available" } else { "You're up to date" }).size(16.).strong());
                });
                ui.add_space(10.);
                ui.label(format!("Game: {}", catalog.game.name));
                ui.label(RichText::new(format!("Build published: {}", catalog.game_asset.updated_at.replace('T', " ").trim_end_matches('Z'))).size(12.).color(MUTED));
                ui.label(RichText::new(format!("Engine: {}", catalog.engine.tag_name)).size(12.).color(MUTED));
                ui.add_space(14.);
                if primary(ui, if self.has_update() { "Install latest build  >" } else { "Verify & install  >" }, !self.busy).clicked() { self.start(Action::InstallGame); }
            } else {
                ui.label(RichText::new("Check for the latest build to see what's new.").color(MUTED));
            }
        });
        if let Some(catalog) = self.catalog.clone() {
            ui.add_space(24.);
            ui.label(RichText::new("Release notes").size(19.).strong());
            ui.add_space(10.);
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                let notes = if catalog.game.body.is_empty() {
                    "No release notes for this build."
                } else {
                    &catalog.game.body
                };
                ui.label(
                    RichText::new(notes.chars().take(16000).collect::<String>())
                        .size(12.)
                        .color(MUTED),
                );
                ui.add_space(12.);
                if ui.link("Read on GitHub >").clicked() {
                    self.open(format!(
                        "{}/releases/tag/{}",
                        github::REPOSITORY,
                        catalog.game.tag_name
                    ));
                }
            });
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Make it feel like yours.",
            "A lightweight launcher, with the controls that matter.",
        );
        ui.add_space(20.);
        let before = serde_json::to_string(&self.settings).unwrap_or_default();
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("PERFORMANCE & UPDATES").size(10.).color(ACCENT));
            ui.add_space(12.);
            ui.checkbox(&mut self.settings.minimize_on_play, "Minimize the launcher while playing");
            ui.checkbox(&mut self.settings.auto_check, "Check for updates at startup and every 15 minutes");
            ui.checkbox(&mut self.settings.update_before_play, "Install the newest build before playing");
            ui.add_space(12.);
            ui.horizontal(|ui| {
                ui.label("Native mod build profile");
                egui::ComboBox::from_id_salt("build_profile").selected_text(self.settings.build_profile.argument()).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.build_profile, BuildProfile::Performance, "Performance · fast incremental rebuilds");
                    ui.selectable_value(&mut self.settings.build_profile, BuildProfile::Release, "Release · thin LTO, slower builds");
                    ui.selectable_value(&mut self.settings.build_profile, BuildProfile::Dev, "Development · quickest build, slower game");
                });
            });
            ui.add_space(6.);
            ui.label(RichText::new("Performance is recommended for everyday play. Rebuild mods after changing this setting.").size(11.).color(MUTED));
        });
        if before != serde_json::to_string(&self.settings).unwrap_or_default() {
            self.save();
        }
        ui.add_space(16.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("ACCOUNTS").size(10.).color(ACCENT));
            ui.add_space(10.);
            for account in self.snapshot.accounts.clone() {
                ui.horizontal(|ui| {
                    self.profile_avatar(ui, &account, 28.);
                    ui.label(RichText::new(&account.name).strong());
                    if account.selected { badge(ui, "ACTIVE", GREEN); }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Change skin").clicked() { self.edit_skin(account.clone()); }
                        if ui.add_enabled(!self.busy, egui::Button::new("Sign out")).clicked() { self.start(Action::SignOut(account.uuid.clone())); }
                        if !account.selected && ui.add_enabled(!self.busy, egui::Button::new("Use account")).clicked() { self.start(Action::SelectAccount(account.uuid.clone())); }
                    });
                });
                ui.add_space(8.);
            }
            if ui.add_enabled(!self.busy, egui::Button::new("Sign in with Microsoft")).clicked() { self.start(Action::SignIn); }
            if self.engine_ready() && !self.snapshot.sign_in_configured {
                ui.label(RichText::new("This engine has no Microsoft application ID configured. Use an official release, or set HORIZON_MSA_CLIENT_ID before starting MCRS.").size(11.).color(ACCENT));
            }
            ui.add_space(6.);
            ui.label(RichText::new("Authentication and credentials are managed by Horizon's official engine and your system keychain.").size(11.).color(MUTED));
        });
        ui.add_space(16.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("STORAGE & SUPPORT").size(10.).color(ACCENT));
            ui.add_space(10.);
            ui.label(
                RichText::new(self.paths.root.display().to_string())
                    .size(11.)
                    .color(MUTED),
            );
            ui.add_space(10.);
            ui.horizontal(|ui| {
                if ui.button("Open game folder").clicked() {
                    self.open(self.game_folder());
                }
                if ui.button("Open data folder").clicked() {
                    self.open(self.paths.root.clone());
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Refresh library"))
                    .clicked()
                {
                    self.start(Action::Refresh);
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Restore engine"))
                    .clicked()
                {
                    // Setup repairs a missing/modified binary using the trusted release digest.
                    self.start(Action::Setup);
                }
            });
        });
        ui.add_space(15.);
        ui.label(RichText::new("Native Rust UI · event-driven rendering · bounded logs · no bundled browser runtime").size(11.).color(MUTED));
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some((url, code)) = self.login.clone() {
            egui::Window::new("Connect your Minecraft account")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0., 0.])
                .default_width(390.)
                .show(ctx, |ui| {
                    ui.add_space(8.);
                    ui.label("Your browser is ready. Enter this code at Microsoft to sign in:");
                    ui.add_space(16.);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&code).monospace().size(30.).color(ACCENT));
                        if ui.button("Copy code").clicked() {
                            ctx.copy_text(code.clone());
                        }
                    });
                    ui.add_space(14.);
                    if ui.link(&url).clicked() {
                        self.open(&url);
                    }
                    ui.add_space(10.);
                    ui.label(
                        RichText::new("Waiting for Microsoft… This window closes after sign-in.")
                            .size(12.)
                            .color(MUTED),
                    );
                    if ui.button("Cancel sign-in").clicked() {
                        self.worker.cancel();
                    }
                });
        }
        if self.confirm.is_some() {
            egui::Window::new("Native mod changes").collapsible(false).resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0., 0.]).default_width(390.).show(ctx, |ui| {
                    let removing = matches!(self.confirm, Some(Confirm::Remove(_)));
                    ui.label(if removing { "Remove this mod and rebuild the game with the remaining mods?" }
                        else { "Updating imports new source code from GitHub. Review the changes before trusting them to run on your computer." });
                    ui.add_space(12.);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() { self.confirm = None; }
                        if ui.add(egui::Button::new(if removing { "Remove & rebuild" } else { "Trust, update & build" }).fill(ACCENT)).clicked()
                            && let Some(confirm) = self.confirm.take() {
                            self.start(match confirm { Confirm::Remove(id) => Action::RemoveMod(id), Confirm::UpdateMods => Action::UpdateMods });
                        }
                    });
                });
        }
    }
}

impl Launcher {
    fn game_folder(&self) -> std::path::PathBuf {
        if self.snapshot.game_home.as_os_str().is_empty() {
            self.paths.game_home()
        } else {
            self.snapshot.game_home.clone()
        }
    }

    fn profile_avatar(&self, ui: &mut egui::Ui, account: &Account, size: f32) {
        if let Some(head) = self.heads.get(&account.uuid) {
            ui.add(egui::Image::new((head.id(), egui::vec2(size, size))).corner_radius(4.));
        } else {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
            ui.painter().rect_filled(rect, 5., LINE);
            let initial = account
                .name
                .chars()
                .next()
                .unwrap_or('?')
                .to_ascii_uppercase()
                .to_string();
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                initial,
                FontId::proportional(size * 0.5),
                MUTED,
            );
        }
    }

    fn edit_skin(&mut self, account: Account) {
        if self.skin_editor.is_some() {
            return;
        }
        let model = if account
            .skin
            .as_ref()
            .is_some_and(|(_, model)| model == "slim")
        {
            SkinModel::Slim
        } else {
            SkinModel::Classic
        };
        self.skin_editor = Some(SkinEditor {
            account,
            model,
            skin: None,
            preview: None,
            choosing: false,
            uploading: false,
        });
    }

    fn skin_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.skin_editor.take() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Change Minecraft skin")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0., 0.])
            .default_width(390.)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(preview) = &editor.preview {
                        ui.add(
                            egui::Image::new((preview.id(), egui::vec2(64., 64.)))
                                .corner_radius(5.),
                        );
                    } else {
                        self.profile_avatar(ui, &editor.account, 64.);
                    }
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&editor.account.name).size(18.).strong());
                        ui.label(RichText::new("Minecraft: Java Edition").color(MUTED));
                    });
                });
                ui.add_space(12.);
                ui.label(
                    "Choose a 64×64 or 64×32 PNG skin. Applying it updates this Minecraft account.",
                );
                ui.add_space(8.);
                if ui
                    .add_enabled(
                        !editor.choosing && !editor.uploading,
                        egui::Button::new("Choose skin PNG…"),
                    )
                    .clicked()
                {
                    match self.profiles.send(ProfileCommand::Choose {
                        uuid: editor.account.uuid.clone(),
                    }) {
                        Ok(()) => editor.choosing = true,
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
                if let Some(skin) = &editor.skin {
                    ui.label(RichText::new(&skin.name).color(GREEN));
                }
                if editor.choosing {
                    ui.label("Waiting for file selection…");
                }
                ui.add_enabled_ui(!editor.uploading, |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(
                            &mut editor.model,
                            SkinModel::Classic,
                            "Classic (wide arms)",
                        );
                        ui.radio_value(&mut editor.model, SkinModel::Slim, "Slim arms");
                    });
                });
                ui.add_space(12.);
                if primary(
                    ui,
                    if editor.uploading {
                        "Applying skin…"
                    } else {
                        "Apply skin"
                    },
                    editor.skin.is_some() && !editor.uploading && !editor.choosing,
                )
                .clicked()
                    && let Some(skin) = editor.skin.clone()
                {
                    match self.profiles.send(ProfileCommand::Upload {
                        account: editor.account.clone(),
                        model: editor.model,
                        skin,
                    }) {
                        Ok(()) => editor.uploading = true,
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
                ui.label(
                    RichText::new("Your new skin appears in new game sessions.")
                        .size(11.)
                        .color(MUTED),
                );
            });
        if open {
            self.skin_editor = Some(editor);
        }
    }
}

impl eframe::App for Launcher {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        for (key, page) in [
            (egui::Key::Num1, Page::Play),
            (egui::Key::Num2, Page::Mods),
            (egui::Key::Num3, Page::Updates),
            (egui::Key::Num4, Page::Settings),
        ] {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, key)) {
                self.page = page;
            }
        }
        self.receive(ctx);
        self.sidebar(ctx);
        self.footer(ctx);
        if self.show_logs {
            egui::TopBottomPanel::bottom("activity")
                .default_height(150.)
                .min_height(70.)
                .resizable(true)
                .frame(
                    Frame::new()
                        .fill(Color32::from_rgb(15, 18, 19))
                        .inner_margin(14),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("ACTIVITY").size(10.).color(ACCENT));
                        if ui.small_button("Copy").clicked() {
                            ctx.copy_text(self.logs.iter().cloned().collect::<Vec<_>>().join("\n"));
                        }
                        if ui.small_button("Clear").clicked() {
                            self.logs.clear();
                        }
                    });
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for line in &self.logs {
                                ui.label(RichText::new(line).monospace().size(10.).color(MUTED));
                            }
                        });
                });
        }
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(30, 24)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(match self.page {
                            Page::Play => "LET'S PLAY",
                            Page::Mods => "YOUR MODS",
                            Page::Updates => "WHAT'S NEW",
                            Page::Settings => "PREFERENCES",
                        })
                        .size(10.)
                        .color(MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        dot(ui, if self.engine_ready() { GREEN } else { MUTED });
                        ui.label(RichText::new("HORIZON ENGINE").size(9.).color(MUTED));
                    });
                });
                ui.add_space(20.);
                if let Some(error) = self.error.clone() {
                    Frame::new()
                        .fill(Color32::from_rgb(63, 36, 32))
                        .inner_margin(12)
                        .corner_radius(6)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_top(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(error)
                                            .size(12.)
                                            .color(Color32::from_rgb(247, 177, 154)),
                                    )
                                    .wrap(),
                                );
                                if ui.small_button("Dismiss").clicked() {
                                    self.error = None;
                                }
                            });
                        });
                    ui.add_space(12.);
                }
                egui::ScrollArea::vertical()
                    .id_salt(match self.page {
                        Page::Play => "play",
                        Page::Mods => "mods",
                        Page::Updates => "updates",
                        Page::Settings => "settings",
                    })
                    .auto_shrink([false; 2])
                    .show(ui, |ui| match self.page {
                        Page::Play => self.play(ui),
                        Page::Mods => self.mods(ui),
                        Page::Updates => self.updates(ui),
                        Page::Settings => self.settings(ui),
                    });
            });
        self.dialogs(ctx);
        self.skin_dialog(ctx);
    }
}

fn configure(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.faint_bg_color = PANEL;
    visuals.selection.bg_fill = Color32::from_rgb(95, 66, 43);
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(39, 45, 44);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(39, 45, 44);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1_f32, LINE);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(62, 62, 49);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(62, 62, 49);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1_f32, ACCENT);
    ctx.set_visuals(visuals);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(9., 9.);
        s.spacing.button_padding = egui::vec2(12., 8.);
        s.text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(13.));
        s.text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(12.));
    });
}

fn card() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1_f32, LINE))
        .corner_radius(9)
        .inner_margin(18)
}

fn primary(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).size(14.).strong().color(BG))
            .fill(ACCENT)
            .corner_radius(5)
            .min_size(egui::vec2(ui.available_width(), 44.)),
    )
}

fn title(ui: &mut egui::Ui, text: &str, description: &str) {
    ui.label(RichText::new(text).size(29.).strong());
    ui.add_space(5.);
    ui.label(RichText::new(description).size(13.).color(MUTED));
}

fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.13))
        .corner_radius(3)
        .inner_margin(Margin::symmetric(7, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(9.).strong().color(color));
        });
}

fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10., 12.), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3., color);
}

fn cube(p: &egui::Painter, center: egui::Pos2, size: f32, color: Color32) {
    let top = center - egui::vec2(0., size * 0.5);
    let right = center + egui::vec2(size, 0.);
    let bottom = center + egui::vec2(0., size * 0.5);
    let left = center - egui::vec2(size, 0.);
    let depth = egui::vec2(0., size);
    p.add(egui::Shape::convex_polygon(
        vec![top, right, bottom, left],
        color,
        Stroke::NONE,
    ));
    p.add(egui::Shape::convex_polygon(
        vec![left, bottom, bottom + depth, left + depth],
        color.gamma_multiply(0.65),
        Stroke::NONE,
    ));
    p.add(egui::Shape::convex_polygon(
        vec![bottom, right, right + depth, bottom + depth],
        color.gamma_multiply(0.82),
        Stroke::NONE,
    ));
}

/// Static vector art is drawn by the native renderer: no images to decode, no animation loop.
fn landscape(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 208.), egui::Sense::hover());
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, CornerRadius::same(9), Color32::from_rgb(38, 54, 48));
    let origin = rect.right_top() + egui::vec2(-150., 58.);
    p.circle_filled(origin, 31., Color32::from_rgb(211, 170, 110));
    p.circle_stroke(
        origin,
        44.,
        Stroke::new(1_f32, Color32::from_rgb(88, 99, 68)),
    );
    for (y, x, w) in [(38., 330., 56.), (70., 100., 43.), (25., 50., 60.)] {
        p.rect_filled(
            egui::Rect::from_min_size(rect.right_top() + egui::vec2(-x, y), egui::vec2(w, 5.)),
            0.,
            Color32::from_rgb(88, 101, 80),
        );
    }
    // Back-to-front rows of isometric grass and stone blocks.
    for row in 0..9 {
        for col in 0..13 {
            let x = rect.right() - 330. + col as f32 * 26. - row as f32 * 23.;
            let y = rect.top() + 121. + row as f32 * 14.;
            let height = ((col * 7 + row * 11) % 5) as f32 * 7.;
            let water = (4..8).contains(&col) && row < 5;
            let color = if water {
                Color32::from_rgb(74, 125, 121)
            } else if (col + row) % 3 == 0 {
                Color32::from_rgb(115, 139, 86)
            } else {
                Color32::from_rgb(94, 123, 76)
            };
            cube(&p, egui::pos2(x, y - height), 24., color);
            if !water && (col * 3 + row * 5) % 17 == 0 {
                let trunk = egui::pos2(x, y - height - 21.);
                cube(&p, trunk, 6., Color32::from_rgb(105, 85, 59));
                cube(
                    &p,
                    trunk - egui::vec2(0., 22.),
                    19.,
                    Color32::from_rgb(54, 90, 63),
                );
                cube(
                    &p,
                    trunk - egui::vec2(0., 39.),
                    14.,
                    Color32::from_rgb(65, 105, 68),
                );
            }
        }
    }
    // Quiet left side keeps the title readable over the landscape.
    p.rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + 300., rect.bottom())),
        9.,
        Color32::from_rgb(38, 54, 48),
    );
    let x = rect.left() + 27.;
    p.text(
        egui::pos2(x, rect.top() + 28.),
        egui::Align2::LEFT_TOP,
        "SAME WORLD. NEW POSSIBILITIES.",
        FontId::proportional(9.),
        GREEN,
    );
    p.text(
        egui::pos2(x, rect.top() + 57.),
        egui::Align2::LEFT_TOP,
        "A familiar world.\nA new horizon.",
        FontId::proportional(32.),
        TEXT,
    );
    p.text(
        egui::pos2(x, rect.bottom() - 31.),
        egui::Align2::LEFT_TOP,
        "Your Minecraft journey, in Rust.",
        FontId::proportional(12.),
        MUTED,
    );
}
