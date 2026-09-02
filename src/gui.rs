//! egui window, "instrument panel" layout: header with mark, path row, component
//! tiles, one Install button, progress, log.

use crate::diagnose;
use crate::game::{self, GameStatus};
use crate::installer::{self, Engine, StepState};
use crate::logo;
use crate::platform;
use crate::theme::{self as t};
use crate::update;
use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Stroke, StrokeKind, Vec2,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

#[derive(Clone)]
enum UpdateState {
    Idle,
    Available(update::Available),
    Downloading(u8, String),
    Restarting,
    Failed(String),
}

enum Msg {
    Progress(u8, String),
    Log(LogLine),
    Finished(Result<String, String>),
}

#[derive(Clone)]
enum LogLine {
    Step(String),
    Ok(String),
    Fail(String),
    Plain(String),
}

/// A game-library row's install state, computed off-thread.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Badge {
    Complete,
    Partial,
    Clean,
    Refused,
    /// No usable Windows exe (native Linux build, empty folder, …).
    NotSupported,
}

impl Badge {
    fn label(self) -> &'static str {
        match self {
            Badge::Complete => "installed",
            Badge::Partial => "partial",
            Badge::Clean => "",
            Badge::Refused => "refused",
            Badge::NotSupported => "not supported",
        }
    }
}

#[derive(Clone)]
struct LibRow {
    entry: platform::GameEntry,
    badge: Badge,
}

pub struct App {
    exe_text: String,
    status: Option<Result<GameStatus, String>>,
    running: bool,
    progress: u8,
    progress_msg: String,
    log: Vec<LogLine>,
    rx: Option<Receiver<Msg>>,
    confirm_remove: bool,
    last_error: Option<String>,
    candidates: Vec<PathBuf>,
    resolved_exe: Option<PathBuf>,
    engine: Engine,
    update: UpdateState,
    update_rx: Option<Receiver<UpdateState>>,
    skipped_version: String,
    library: Vec<LibRow>,
    library_rx: Option<Receiver<LibRow>>,
    lib_filter: String,
    /// Launch-option outcome to show: (game dir it applies to, the advice).
    launch_panel: Option<(PathBuf, platform::LaunchAdvice)>,
    /// The running worker is an install (not a removal): apply options after.
    finishing_install: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        t::install(&cc.egui_ctx);
        logo::set_taskbar_icon(cc);
        let exe_text = cc
            .storage
            .and_then(|s| s.get_string("exe"))
            .unwrap_or_default();
        let mut app = App {
            exe_text,
            status: None,
            running: false,
            progress: 0,
            progress_msg: String::new(),
            log: Vec::new(),
            rx: None,
            confirm_remove: false,
            last_error: None,
            candidates: Vec::new(),
            resolved_exe: None,
            engine: Engine::default(),
            update: UpdateState::Idle,
            update_rx: None,
            skipped_version: cc
                .storage
                .and_then(|s| s.get_string("skip_version"))
                .unwrap_or_default(),
            library: Vec::new(),
            library_rx: None,
            lib_filter: String::new(),
            launch_panel: None,
            finishing_install: false,
        };
        app.refresh();
        app.start_update_check();
        app.start_library_scan();
        app
    }

    /// Enumerate launcher games on a worker thread; rows stream in as their
    /// install state is inspected (big game dirs make that slow).
    fn start_library_scan(&mut self) {
        let (tx, rx): (Sender<LibRow>, Receiver<LibRow>) = channel();
        self.library.clear();
        self.library_rx = Some(rx);
        thread::spawn(move || {
            for entry in platform::scan_all() {
                let badge = match game::resolve_target(&entry.dir) {
                    Err(_) => Badge::NotSupported,
                    Ok((exe, _)) => match game::inspect(&exe) {
                        Err(_) => Badge::NotSupported,
                        Ok(st) if !st.problems.is_empty() => Badge::Refused,
                        Ok(st) if st.complete() => Badge::Complete,
                        Ok(st)
                            if st.reshade
                                || st.feeder
                                || st.dlss5_addon
                                || st.dlssnr
                                || st.opti =>
                        {
                            Badge::Partial
                        }
                        Ok(_) => Badge::Clean,
                    },
                };
                if tx.send(LibRow { entry, badge }).is_err() {
                    return;
                }
            }
        });
    }

    fn pump_library(&mut self) {
        let Some(rx) = &self.library_rx else { return };
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(row) => self.library.push(row),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            self.library_rx = None;
        }
    }

    /// The game-library picker: one row per launcher game, click to select.
    fn library_panel(&mut self, ui: &mut egui::Ui) {
        self.pump_library();
        if self.library_rx.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
        if self.library.is_empty() {
            return;
        }
        egui::Panel::top("library")
            .frame(
                Frame::new()
                    .fill(t::PANEL)
                    .inner_margin(Margin {
                        left: 18,
                        right: 28,
                        top: 10,
                        bottom: 8,
                    })
                    .stroke(Stroke::new(1.0, t::BORDER)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("GAME LIBRARY")
                            .font(t::plex_semibold(10.5))
                            .color(t::TEXT_MUTED),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.lib_filter)
                            .hint_text(RichText::new("filter").color(t::TEXT_DIM))
                            .font(t::plex(11.0))
                            .desired_width(140.0),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Rescan").clicked() {
                            self.start_library_scan();
                        }
                        if self.library_rx.is_some() {
                            ui.label(
                                RichText::new("scanning…")
                                    .font(t::plex(10.5))
                                    .color(t::TEXT_DIM),
                            );
                        }
                    });
                });
                ui.add_space(4.0);
                let filter = self.lib_filter.to_lowercase();
                let selected = self.input_path();
                let mut clicked: Option<PathBuf> = None;
                egui::ScrollArea::vertical()
                    .max_height(126.0)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for row in &self.library {
                            if !filter.is_empty()
                                && !row.entry.name.to_lowercase().contains(&filter)
                            {
                                continue;
                            }
                            let is_sel = selected == row.entry.dir;
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [52.0, 16.0],
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "[{}]",
                                            row.entry.launcher.label()
                                        ))
                                        .font(t::mono(10.0))
                                        .color(t::TEXT_DIM),
                                    ),
                                );
                                let resp = ui.selectable_label(
                                    is_sel,
                                    RichText::new(&row.entry.name)
                                        .font(t::plex_medium(12.0))
                                        .color(if is_sel { t::TEXT } else { t::TEXT_SOFT }),
                                );
                                if resp.clicked() {
                                    clicked = Some(row.entry.dir.clone());
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let col = match row.badge {
                                        Badge::Complete => t::ACCENT,
                                        Badge::Partial => t::TEXT_OFF,
                                        Badge::Refused => t::DANGER,
                                        _ => t::TEXT_DIM,
                                    };
                                    let label = row.badge.label();
                                    if !label.is_empty() {
                                        ui.label(
                                            RichText::new(label).font(t::plex(10.5)).color(col),
                                        );
                                    }
                                });
                            });
                        }
                    });
                if let Some(dir) = clicked {
                    self.exe_text = dir.to_string_lossy().into_owned();
                    self.launch_panel = None;
                    self.refresh();
                }
            });
    }

    /// The post-install / on-demand launch-options bar.
    fn launch_options_panel(&mut self, ui: &mut egui::Ui) {
        let Some((dir, advice)) = self.launch_panel.clone() else {
            return;
        };
        use platform::LaunchAdvice as A;
        let mut dismiss = false;
        let mut retry = false;
        egui::Panel::top("launch_options")
            .frame(
                Frame::new()
                    .fill(t::TILE)
                    .inner_margin(Margin {
                        left: 18,
                        right: 28,
                        top: 10,
                        bottom: 10,
                    })
                    .stroke(Stroke::new(1.0, t::BORDER_STRONG)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("LAUNCH OPTIONS")
                            .font(t::plex_semibold(10.5))
                            .color(t::TEXT_MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Dismiss").clicked() {
                            dismiss = true;
                        }
                    });
                });
                ui.add_space(4.0);
                let copy_row = |ui: &mut egui::Ui, text: &str| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(text).font(t::mono(11.0)).color(t::TEXT_SOFT),
                        );
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(text.to_string());
                        }
                    });
                };
                match &advice {
                    A::AppliedSteam(outcomes) => {
                        ui.label(
                            RichText::new(
                                "Steam launch options set — restart Steam to pick them up.",
                            )
                            .font(t::plex_medium(12.0))
                            .color(t::TEXT),
                        );
                        for o in outcomes {
                            copy_row(ui, &o.merged);
                            ui.label(
                                RichText::new(format!("backup: {}", o.backup.display()))
                                    .font(t::plex(10.5))
                                    .color(t::TEXT_DIM),
                            );
                        }
                    }
                    A::AlreadySet => {
                        ui.label(
                            RichText::new("Steam launch options already contain everything needed.")
                                .font(t::plex_medium(12.0))
                                .color(t::TEXT),
                        );
                    }
                    A::ManualSteam { display, why } => {
                        ui.label(
                            RichText::new(format!("Not applied automatically: {why}"))
                                .font(t::plex(11.5))
                                .color(t::TEXT_SOFT),
                        );
                        ui.label(
                            RichText::new(
                                "Paste into Steam: right-click the game → Properties → Launch Options — or close Steam and retry:",
                            )
                            .font(t::plex(11.5))
                            .color(t::TEXT_SOFT),
                        );
                        copy_row(ui, display);
                        if ui.button("Retry (Steam closed)").clicked() {
                            retry = true;
                        }
                    }
                    A::AppliedHeroic { file } => {
                        ui.label(
                            RichText::new(format!(
                                "Heroic environment variables set ({}). Restart Heroic.",
                                file.display()
                            ))
                            .font(t::plex_medium(12.0))
                            .color(t::TEXT),
                        );
                    }
                    A::ManualEnv {
                        launcher,
                        vars,
                        why,
                    } => {
                        if let Some(why) = why {
                            ui.label(
                                RichText::new(why).font(t::plex(11.5)).color(t::TEXT_SOFT),
                            );
                        }
                        ui.label(
                            RichText::new(format!(
                                "Add these in {}'s per-game environment settings:",
                                launcher.label()
                            ))
                            .font(t::plex(11.5))
                            .color(t::TEXT_SOFT),
                        );
                        let joined = vars
                            .iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        copy_row(ui, &joined);
                    }
                    A::UnknownLauncher { display } => {
                        ui.label(
                            RichText::new(
                                "This folder belongs to no known launcher. Under Proton/Wine the game needs:",
                            )
                            .font(t::plex(11.5))
                            .color(t::TEXT_SOFT),
                        );
                        copy_row(ui, display);
                    }
                }
            });
        if retry {
            let advice = platform::ensure_launch_options(&dir, self.engine, false);
            self.launch_panel = Some((dir, advice));
        }
        if dismiss {
            self.launch_panel = None;
        }
    }

    fn exe(&self) -> Option<PathBuf> {
        self.resolved_exe.clone()
    }

    fn input_path(&self) -> PathBuf {
        PathBuf::from(self.exe_text.trim().trim_matches('"'))
    }

    /// Text box accepts an exe or a game folder; a folder is resolved to its game exe.
    fn refresh(&mut self) {
        let input = self.input_path();
        let prev = self.resolved_exe.take();
        self.candidates.clear();
        if input.as_os_str().is_empty() {
            self.status = None;
            return;
        }
        match game::resolve_target(&input) {
            Ok((exe, cands)) => {
                self.resolved_exe = Some(match prev {
                    Some(p) if cands.len() > 1 && cands.contains(&p) => p,
                    _ => exe,
                });
                self.candidates = cands;
            }
            Err(e) => {
                self.status = Some(Err(format!("{e:#}")));
                return;
            }
        }
        self.inspect_resolved();
    }

    fn inspect_resolved(&mut self) {
        self.status = self
            .resolved_exe
            .as_ref()
            .map(|p| game::inspect(p).map_err(|e| format!("{e:#}")));
    }

    fn start(&mut self, remove: Option<bool>) {
        let Some(exe) = self.exe() else { return };
        let engine = self.engine;
        self.finishing_install = remove.is_none();
        self.launch_panel = None;
        let (tx, rx): (Sender<Msg>, Receiver<Msg>) = channel();
        self.rx = Some(rx);
        self.running = true;
        self.progress = 0;
        self.progress_msg.clear();
        self.log.clear();
        self.last_error = None;
        thread::spawn(move || {
            let out = if let Some(everything) = remove {
                let res = if everything {
                    installer::uninstall_all(&exe).map(|(mut r, kept)| {
                        r.extend(kept);
                        r
                    })
                } else {
                    installer::uninstall(&exe)
                };
                res.map(|r| {
                    format!(
                        "Removed: {}",
                        if r.is_empty() {
                            "nothing".into()
                        } else {
                            r.join(", ")
                        }
                    )
                })
                .map_err(|e| format!("{e:#}"))
            } else {
                let p_tx = tx.clone();
                let s_tx = tx.clone();
                installer::run_all_with(
                    &exe,
                    engine,
                    &move |pct, msg| {
                        let _ = p_tx.send(Msg::Progress(pct, msg.to_owned()));
                    },
                    &move |i, n, name, state, detail| {
                        let line = match state {
                            StepState::Start => LogLine::Step(format!("[{}/{n}] {name}", i + 1)),
                            StepState::Done => LogLine::Ok(format!("ok: {detail}")),
                            StepState::Error => LogLine::Fail(format!("FAILED: {detail}")),
                        };
                        let _ = s_tx.send(Msg::Log(line));
                    },
                )
                .map(|_| {
                    if engine == Engine::Opti {
                        "Done. In game: Insert opens the OptiScaler overlay → enable Neural Rendering.".to_owned()
                    } else {
                        "Done. In game: Home opens ReShade → Add-ons tab → DLSS 5 Neural Rendering → enable. (Home tab saying \"no effect files\" is normal on games with their own DLSS.)".to_owned()
                    }
                })
                .map_err(|e| format!("{e:#}"))
            };
            let _ = tx.send(Msg::Finished(out));
        });
    }

    fn start_update_check(&mut self) {
        let (tx, rx) = channel::<UpdateState>();
        self.update_rx = Some(rx);
        thread::spawn(move || {
            let st = match update::check() {
                Ok(Some(av)) => UpdateState::Available(av),
                _ => UpdateState::Idle,
            };
            let _ = tx.send(st);
        });
    }

    fn start_update_download(&mut self, av: update::Available) {
        let (tx, rx) = channel::<UpdateState>();
        self.update_rx = Some(rx);
        self.update = UpdateState::Downloading(0, "Starting".into());
        thread::spawn(move || {
            let p_tx = tx.clone();
            let res = update::download_and_swap(&av, &move |pct, msg| {
                let _ = p_tx.send(UpdateState::Downloading(pct, msg.to_owned()));
            });
            match res {
                Ok(exe) => {
                    let _ = tx.send(UpdateState::Restarting);
                    if update::relaunch(&exe).is_ok() {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        std::process::exit(0);
                    }
                }
                Err(e) => {
                    let _ = tx.send(UpdateState::Failed(format!("{e:#}")));
                }
            }
        });
    }

    fn run_diagnose(&mut self) {
        let Some(exe) = self.exe() else { return };
        self.log.clear();
        self.progress_msg.clear();
        match diagnose::run_full(&exe) {
            Ok(findings) => {
                for f in findings {
                    let line = match f.level {
                        diagnose::Level::Ok => LogLine::Ok(format!("ok: {}", f.text)),
                        diagnose::Level::Warn => LogLine::Plain(format!("warn: {}", f.text)),
                        diagnose::Level::Bad => LogLine::Fail(format!("FAIL: {}", f.text)),
                    };
                    self.log.push(line);
                }
            }
            Err(e) => self.log.push(LogLine::Fail(format!("{e:#}"))),
        }
    }

    fn pump_update(&mut self) {
        let Some(rx) = &self.update_rx else { return };
        let mut last = None;
        while let Ok(m) = rx.try_recv() {
            last = Some(m);
        }
        if let Some(m) = last {
            let skip =
                matches!(&m, UpdateState::Available(av) if av.version == self.skipped_version);
            self.update = if skip { UpdateState::Idle } else { m };
        }
    }

    fn pump(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut finished = None;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Progress(p, s) => {
                    self.progress = p;
                    self.progress_msg = s;
                }
                Msg::Log(l) => self.log.push(l),
                Msg::Finished(r) => finished = Some(r),
            }
        }
        if let Some(r) = finished {
            self.rx = None;
            self.running = false;
            match r {
                Ok(msg) => {
                    self.progress = 100;
                    self.progress_msg = "Done.".into();
                    self.log.push(LogLine::Plain(msg));
                    if cfg!(target_os = "linux") && self.finishing_install {
                        if let Some(dir) =
                            self.exe().and_then(|e| e.parent().map(Path::to_path_buf))
                        {
                            let advice =
                                platform::ensure_launch_options(&dir, self.engine, false);
                            self.launch_panel = Some((dir, advice));
                        }
                    }
                }
                Err(e) => {
                    self.progress_msg = "Failed.".into();
                    self.log.push(LogLine::Fail(e.clone()));
                    self.last_error = Some(e);
                }
            }
            self.finishing_install = false;
            self.refresh();
            self.start_library_scan();
        }
    }
}

struct Tile {
    title: &'static str,
    detail: &'static str,
    ok: fn(&GameStatus) -> bool,
    optional: bool,
}

const TILES_FEEDER: [Tile; 6] = [
    Tile {
        title: "ReShade",
        detail: "add-on build · dxgi.dll",
        ok: |s| s.reshade,
        optional: false,
    },
    Tile {
        title: "Shader headers",
        detail: "ReShade.fxh · ReShadeUI.fxh · DrawText.fxh",
        ok: |s| s.headers,
        optional: false,
    },
    Tile {
        title: "DLSS5-Feeder",
        detail: "dlss5-feed.addon64 · DLSS5_Feed.fx",
        ok: |s| s.feeder,
        optional: false,
    },
    Tile {
        title: "LumeniteFX",
        detail: "motion vectors · Kernel 2.0",
        ok: |s| s.lumenite,
        optional: false,
    },
    Tile {
        title: "DLSS 5 add-on · leaked",
        detail: "renodx-dlss5.addon64 · nvngx_dlssnr.dll",
        ok: |s| s.dlss5_addon && s.dlssnr,
        optional: false,
    },
    Tile {
        title: "nvngx_dlss.dll",
        detail: "DLSS runtime · the Feeder's NGX session needs it",
        ok: |s| s.dlss,
        optional: false,
    },
];

const TILE_OPTI: Tile = Tile {
    title: "OptiScaler + NR pass",
    detail: "Dagherbou fork as dxgi.dll · Insert opens its overlay",
    ok: |s| s.opti,
    optional: false,
};

const TILES_NATIVE: [Tile; 4] = [
    Tile {
        title: "Game DLSS",
        detail: "nvngx_dlss.dll shipped by the game · add-on hooks it directly",
        ok: |_| true,
        optional: false,
    },
    Tile {
        title: "ReShade",
        detail: "add-on build · dxgi.dll",
        ok: |s| s.reshade,
        optional: false,
    },
    Tile {
        title: "DLSS 5 add-on · leaked",
        detail: "renodx-dlss5.addon64 · nvngx_dlssnr.dll",
        ok: |s| s.dlss5_addon && s.dlssnr,
        optional: false,
    },
    Tile {
        title: "DX11 bridge",
        detail: "dlss5-bridge.addon64 (NIGos) · D3D11 games",
        ok: |s| s.bridge,
        optional: true,
    },
];

fn tiles_for(st: Option<&GameStatus>) -> Vec<&'static Tile> {
    match st.map(|s| s.mode) {
        Some(game::Mode::Native) if st.is_some_and(|s| s.opti) => {
            vec![&TILES_NATIVE[0], &TILE_OPTI, &TILES_NATIVE[2]]
        }
        Some(game::Mode::Native) => {
            let needs_bridge = st.is_some_and(|s| s.needs_bridge());
            TILES_NATIVE
                .iter()
                .filter(|t| t.title != "DX11 bridge" || needs_bridge)
                .collect()
        }
        _ => TILES_FEEDER.iter().collect(),
    }
}

fn chip(ui: &mut egui::Ui, text: &str, color: Color32, outlined: bool) {
    Frame::new()
        .stroke(Stroke::new(1.0, if outlined { color } else { t::BORDER }))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(t::mono(10.0)).color(color));
        });
}

fn paint_check(ui: &mut egui::Ui, ok: bool, optional: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    if ok {
        p.circle_filled(c, 8.0, t::ACCENT);
        let s = Stroke::new(1.8, t::BG);
        p.line_segment([c + Vec2::new(-3.5, 0.2), c + Vec2::new(-1.2, 2.5)], s);
        p.line_segment([c + Vec2::new(-1.2, 2.5), c + Vec2::new(3.5, -2.3)], s);
    } else {
        p.circle_stroke(
            c,
            7.2,
            Stroke::new(1.6, if optional { t::RING_OFF } else { t::TEXT_DIM }),
        );
    }
}

fn tile(ui: &mut egui::Ui, rect: egui::Rect, tl: &Tile, st: Option<&GameStatus>) {
    let ok = st.map(tl.ok).unwrap_or(false);
    let dashed = tl.optional && !ok;
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), t::TILE);
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, if dashed { t::BORDER_DASH } else { t::BORDER }),
        StrokeKind::Inside,
    );
    let inner = rect.shrink2(Vec2::new(11.0, 9.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            paint_check(ui, ok, tl.optional);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                let title_color = if dashed { t::TEXT_OFF } else { t::TEXT };
                ui.label(
                    RichText::new(tl.title)
                        .font(t::plex_medium(13.0))
                        .color(title_color),
                );
                ui.label(
                    RichText::new(tl.detail)
                        .font(t::plex(11.0))
                        .color(if dashed { t::TEXT_DIM } else { t::TEXT_MUTED }),
                );
            });
        },
    );
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("exe", self.exe_text.clone());
        storage.set_string("skip_version", self.skipped_version.clone());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        if self.running {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }

        let (ok_status, problems, complete) = match &self.status {
            Some(Ok(s)) => (Some(s.clone()), s.problems.clone(), s.complete()),
            Some(Err(e)) => (None, vec![e.clone()], false),
            None => (None, vec![], false),
        };

        // ── header ────────────────────────────────────────────────
        egui::Panel::top("header")
            .frame(
                Frame::new()
                    .fill(t::HEADER)
                    .inner_margin(Margin {
                        left: 18,
                        right: 28,
                        top: 12,
                        bottom: 12,
                    })
                    .stroke(Stroke::new(1.0, t::BORDER)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), egui::Sense::hover());
                    logo::paint_mark(ui.painter(), rect, t::ACCENT, t::BG);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(
                            RichText::new("DLSS5oneclick")
                                .font(t::sora(15.0))
                                .color(t::TEXT),
                        );
                        ui.label(
                            RichText::new("Leaked DLSS 5 neural rendering for any DX11/DX12 game")
                                .font(t::plex(11.0))
                                .color(t::TEXT_MUTED),
                        );
                    });
                    chip(ui, "LEAKED BUILD", t::ACCENT, true);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        chip(
                            ui,
                            concat!("v", env!("CARGO_PKG_VERSION")),
                            t::TEXT_DIM,
                            false,
                        );
                    });
                });
            });

        self.pump_update();
        match self.update.clone() {
            UpdateState::Idle => {}
            UpdateState::Available(av) => {
                egui::Panel::top("update_bar")
                    .frame(
                        Frame::new()
                            .fill(t::TILE)
                            .inner_margin(Margin {
                                left: 18,
                                right: 28,
                                top: 8,
                                bottom: 8,
                            })
                            .stroke(Stroke::new(1.0, t::BORDER)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            ui.label(
                                RichText::new(format!(
                                    "Version {} is available (you have {}).",
                                    av.version,
                                    update::CURRENT
                                ))
                                .font(t::plex_medium(12.5))
                                .color(t::TEXT),
                            );
                            let upd = egui::Button::new(
                                RichText::new("Update")
                                    .font(t::plex_semibold(12.5))
                                    .color(t::BG),
                            )
                            .fill(t::ACCENT)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6));
                            if ui.add(upd).clicked() {
                                self.start_update_download(av.clone());
                            }
                            if ui.button("Later").clicked() {
                                self.update = UpdateState::Idle;
                            }
                            if ui.button("Skip this version").clicked() {
                                self.skipped_version = av.version.clone();
                                self.update = UpdateState::Idle;
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.hyperlink_to(
                                    RichText::new("release notes").font(t::plex(11.5)),
                                    format!(
                                        "https://github.com/{}/releases/tag/{}",
                                        update::REPO,
                                        av.tag
                                    ),
                                );
                            });
                        });
                    });
            }
            UpdateState::Downloading(pct, msg) => {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
                egui::Panel::top("update_bar")
                    .frame(
                        Frame::new()
                            .fill(t::TILE)
                            .inner_margin(Margin {
                                left: 18,
                                right: 28,
                                top: 8,
                                bottom: 8,
                            })
                            .stroke(Stroke::new(1.0, t::BORDER)),
                    )
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("Updating: {pct}% {msg}"))
                                .font(t::plex(12.0))
                                .color(t::TEXT_MUTED),
                        );
                    });
            }
            UpdateState::Restarting => {
                egui::Panel::top("update_bar")
                    .frame(
                        Frame::new()
                            .fill(t::TILE)
                            .inner_margin(Margin {
                                left: 18,
                                right: 28,
                                top: 8,
                                bottom: 8,
                            })
                            .stroke(Stroke::new(1.0, t::BORDER)),
                    )
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Updated. Restarting...")
                                .font(t::plex(12.0))
                                .color(t::ACCENT),
                        );
                    });
            }
            UpdateState::Failed(e) => {
                egui::Panel::top("update_bar")
                    .frame(
                        Frame::new()
                            .fill(t::TILE)
                            .inner_margin(Margin {
                                left: 18,
                                right: 28,
                                top: 8,
                                bottom: 8,
                            })
                            .stroke(Stroke::new(1.0, t::BORDER)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Update failed: {e}"))
                                    .font(t::plex(12.0))
                                    .color(t::DANGER),
                            );
                            if ui.button("Dismiss").clicked() {
                                self.update = UpdateState::Idle;
                            }
                        });
                    });
            }
        }

        self.library_panel(ui);
        self.launch_options_panel(ui);

        egui::CentralPanel::default()
            .frame(Frame::new().fill(t::PANEL).inner_margin(Margin {
                left: 18,
                right: 28,
                top: 14,
                bottom: 14,
            }))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;

                // ── path row ──────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let btn_w = 2.0 * 96.0 + 8.0;
                    let field_w = ui.available_width() - btn_w - 8.0;
                    Frame::new()
                        .fill(t::BG)
                        .stroke(Stroke::new(1.0, t::BORDER_STRONG))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 0))
                        .show(ui, |ui| {
                            ui.set_min_height(40.0);
                            ui.set_width(field_w - 24.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut self.exe_text)
                                        .frame(Frame::NONE)
                                        .font(t::mono(12.0))
                                        .text_color(t::TEXT_SOFT)
                                        .hint_text(RichText::new("Game folder or the game's .exe").color(t::TEXT_DIM))
                                        .desired_width(f32::INFINITY),
                                );
                                if r.changed() {
                                    self.refresh();
                                }
                            });
                        });
                    let start_dir = self.exe().and_then(|p| p.parent().map(|d| d.to_path_buf()));
                    if ui.add_sized([96.0, 40.0], egui::Button::new("Game folder…")).clicked() {
                        let mut dlg = rfd::FileDialog::new().set_title("Pick the game's install folder");
                        if let Some(d) = &start_dir { dlg = dlg.set_directory(d); }
                        if let Some(p) = dlg.pick_folder() {
                            self.exe_text = p.to_string_lossy().into_owned();
                            self.refresh();
                        }
                    }
                    if ui.add_sized([96.0, 40.0], egui::Button::new("Exe…")).clicked() {
                        let mut dlg = rfd::FileDialog::new().add_filter("Executables", &["exe"]);
                        if let Some(d) = &start_dir { dlg = dlg.set_directory(d); }
                        if let Some(p) = dlg.pick_file() {
                            self.exe_text = p.to_string_lossy().into_owned();
                            self.refresh();
                        }
                    }
                });

                // ── game exe line ─────────────────────────────────
                if let Some(exe) = self.resolved_exe.clone() {
                    let base = self.input_path();
                    let short = |p: &PathBuf| -> String {
                        p.strip_prefix(&base)
                            .map(|r| r.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                    };
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(RichText::new("Game exe").font(t::plex(12.0)).color(t::TEXT_MUTED));
                        if self.candidates.len() > 1 {
                            let mut pick = exe.clone();
                            egui::ComboBox::from_id_salt("exe_pick")
                                .selected_text(RichText::new(short(&pick)).font(t::mono(11.5)))
                                .show_ui(ui, |ui| {
                                    for c in &self.candidates {
                                        ui.selectable_value(&mut pick, c.clone(), short(c));
                                    }
                                });
                            if pick != exe {
                                self.resolved_exe = Some(pick);
                                self.inspect_resolved();
                            }
                        } else {
                            chip(ui, &short(&exe), t::TEXT_SOFT, false);
                        }
                        if let Some(s) = &ok_status {
                            let mode = match s.mode {
                                game::Mode::Native => "native DLSS · add-on hooks the game",
                                game::Mode::Feeder => "no DLSS · Feeder path",
                            };
                            ui.label(
                                RichText::new(format!(
                                    "{}-bit · {} · {mode}{}",
                                    s.bitness,
                                    s.api.label(),
                                    s.gpu.as_ref().map(|(g, t)| format!(" · {} ({})", g.name, t.label())).unwrap_or_default()
                                ))
                                    .font(t::plex(12.0))
                                    .color(t::TEXT_DIM),
                            );
                        }
                    });
                }
                for p in &problems {
                    ui.label(RichText::new(p).font(t::plex(12.0)).color(t::DANGER));
                }

                // ── tiles ─────────────────────────────────────────
                let gap = 8.0;
                let tile_h = 58.0;
                let row_w = ui.available_width();
                let col_w = ((row_w - gap) / 2.0).floor();
                let tiles = tiles_for(ok_status.as_ref());
                for row in tiles.chunks(2) {
                    let (row_rect, _) =
                        ui.allocate_exact_size(Vec2::new(row_w, tile_h), egui::Sense::hover());
                    for (i, tl) in row.iter().enumerate() {
                        let x = row_rect.left() + i as f32 * (col_w + gap);
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(x, row_rect.top()),
                            Vec2::new(col_w, tile_h),
                        );
                        tile(ui, rect, tl, ok_status.as_ref());
                    }
                }

                // ── engine choice (games with native DLSS) ────────
                if ok_status.as_ref().is_some_and(|s| s.mode == game::Mode::Native) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.label(RichText::new("Engine").font(t::plex(12.0)).color(t::TEXT_MUTED));
                        ui.radio_value(&mut self.engine, Engine::ReShade, "ReShade + RenoDX add-on");
                        ui.radio_value(&mut self.engine, Engine::Opti, "OptiScaler (built-in NR pass)");
                    });
                    if self.engine == Engine::Opti {
                        ui.label(
                            RichText::new(
                                "OptiScaler engine: extracts Dagherbou's OptiScaler_DLSSNR fork as dxgi.dll and adds the DLSS 5 model.                                  In game: Insert opens the OptiScaler overlay, enable Neural Rendering there (off by default).                                  Not compatible with a ReShade install in the same game.",
                            )
                            .font(t::plex(11.0))
                            .color(t::TEXT_DIM),
                        );
                    }
                } else {
                    self.engine = Engine::ReShade;
                }

                // ── actions ───────────────────────────────────────
                let can_run = ok_status.is_some() && problems.is_empty() && !self.running;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let install = egui::Button::new(RichText::new("Install DLSS 5").font(t::plex_semibold(14.0)).color(t::BG))
                        .fill(t::ACCENT)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(180.0, 42.0));
                    if ui.add_enabled(can_run, install).clicked() {
                        self.start(None);
                    }
                    let remove = egui::Button::new(RichText::new("Remove").font(t::plex_medium(13.0)).color(t::TEXT_OFF))
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0, t::BORDER_STRONG))
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(90.0, 42.0));
                    if ui.add_enabled(ok_status.is_some() && !self.running, remove).clicked() {
                        self.confirm_remove = true;
                    }
                    let diag = egui::Button::new(
                        RichText::new("Diagnose").font(t::plex_medium(13.0)).color(t::TEXT_OFF),
                    )
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::new(1.0, t::BORDER_STRONG))
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(96.0, 42.0));
                    if ui
                        .add_enabled(ok_status.is_some() && !self.running, diag)
                        .on_hover_text(
                            "Reads this game's ReShade and feed logs and says why neural rendering is or is not running. Play the game first.",
                        )
                        .clicked()
                    {
                        self.run_diagnose();
                    }
                    if cfg!(target_os = "linux") {
                        let lo = egui::Button::new(
                            RichText::new("Launch options").font(t::plex_medium(13.0)).color(t::TEXT_OFF),
                        )
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0, t::BORDER_STRONG))
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(120.0, 42.0));
                        if ui
                            .add_enabled(ok_status.is_some() && !self.running, lo)
                            .on_hover_text(
                                "Sets the WINEDLLOVERRIDES/Proton launch options this game needs (Steam: applied for you when Steam is closed).",
                            )
                            .clicked()
                        {
                            if let Some(dir) = self.exe().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
                                let advice = platform::ensure_launch_options(&dir, self.engine, false);
                                self.launch_panel = Some((dir, advice));
                            }
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let msg = if self.running || !self.progress_msg.is_empty() {
                            self.progress_msg.clone()
                        } else if complete {
                            "Everything is in place.".to_owned()
                        } else {
                            String::new()
                        };
                        let color = if msg.starts_with("Failed") { t::DANGER } else { t::ACCENT };
                        ui.label(RichText::new(msg).font(t::plex_medium(12.0)).color(color));
                    });
                });

                // ── progress ──────────────────────────────────────
                let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), egui::Sense::hover());
                ui.painter().rect_filled(bar, CornerRadius::same(2), t::BORDER);
                let frac = if self.running { self.progress as f32 / 100.0 } else if self.progress == 100 || complete { 1.0 } else { 0.0 };
                if frac > 0.0 {
                    let mut fill = bar;
                    fill.set_width(bar.width() * frac);
                    ui.painter().rect_filled(fill, CornerRadius::same(2), t::ACCENT);
                }

                // ── log ───────────────────────────────────────────
                let hint_h = 34.0;
                let log_h = (ui.available_height() - hint_h - 12.0).max(80.0);
                Frame::new()
                    .fill(t::BG)
                    .stroke(Stroke::new(1.0, t::BORDER))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_height(log_h - 20.0);
                        egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.set_width(ui.available_width());
                            for l in &self.log {
                                match l {
                                    LogLine::Step(s) => {
                                        let (idx, rest) = s.split_once(' ').unwrap_or(("", s));
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 6.0;
                                            ui.label(RichText::new(idx).font(t::mono(11.5)).color(t::TEXT_DIM));
                                            ui.label(RichText::new(rest).font(t::mono(11.5)).color(t::TEXT_MUTED));
                                        });
                                    }
                                    LogLine::Ok(s) => { ui.label(RichText::new(format!("      {s}")).font(t::mono(11.5)).color(t::ACCENT)); }
                                    LogLine::Fail(s) => { ui.label(RichText::new(format!("      {s}")).font(t::mono(11.5)).color(t::DANGER)); }
                                    LogLine::Plain(s) => { ui.label(RichText::new(s).font(t::mono(11.5)).color(t::TEXT)); }
                                }
                            }
                        });
                    });

                ui.label(RichText::new(
                    "After install, in game: press Home for the ReShade overlay, open the DLSS 5 Neural Rendering panel and enable it. \
                     Keep MSAA/SSAA off. Check dlss5-feed.log next to the exe for 'feature ready'.")
                    .font(t::plex(11.0)).color(t::TEXT_DIM));
            });

        // ── dialogs ───────────────────────────────────────────────
        if self.confirm_remove {
            egui::Window::new(RichText::new("Remove DLSS 5 files").font(t::sora(14.0)))
                .collapsible(false).resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.label("Remove the DLSS 5 files from this game?

Remove takes out what this tool added; ReShade stays.
Remove incl. ReShade also deletes ReShade (dxgi.dll, ini files, reshade-shaders) - refused if any add-on or shader this tool did not install is still there.");
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() { self.confirm_remove = false; self.start(Some(false)); }
                        if ui.button("Remove incl. ReShade").clicked() { self.confirm_remove = false; self.start(Some(true)); }
                        if ui.button("Cancel").clicked() { self.confirm_remove = false; }
                    });
                });
        }
        if let Some(err) = self.last_error.clone() {
            egui::Window::new(RichText::new("DLSS5oneclick").font(t::sora(14.0)))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.set_max_width(480.0);
                    ui.label(RichText::new(&err).color(t::DANGER));
                    if ui.button("OK").clicked() {
                        self.last_error = None;
                    }
                });
        }
    }
}

pub fn run() -> eframe::Result {
    #[cfg(target_os = "linux")]
    const TITLE: &str = concat!("DLSS5oneclick ", env!("CARGO_PKG_VERSION"), " · for Linux");
    #[cfg(not(target_os = "linux"))]
    const TITLE: &str = concat!("DLSS5oneclick ", env!("CARGO_PKG_VERSION"));
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([760.0, 680.0])
        .with_min_inner_size([700.0, 560.0])
        .with_title(TITLE);
    if let Some(icon) = logo::icon_data() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        concat!("DLSS5oneclick ", env!("CARGO_PKG_VERSION")),
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
