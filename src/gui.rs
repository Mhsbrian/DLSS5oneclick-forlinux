//! egui window, "instrument panel" layout: header with mark, path row, component
//! tiles, one Install button, progress, log.

use crate::diagnose;
use crate::game::{self, GameStatus};
use crate::installer::{self, Engine, StepState};
use crate::logo;
use crate::net;
use crate::renodx;
use crate::theme::{self as t};
use crate::update;
use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Stroke, StrokeKind, Vec2,
};
use std::path::PathBuf;
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
    /// "Also install the RenoDX HDR mod" checkbox.
    renodx_on: bool,
    renodx: RenodxLookup,
    renodx_rx: Option<Receiver<RenodxLookup>>,
    /// Exe the current lookup belongs to, so a refresh does not re-fetch.
    renodx_for: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum RenodxLookup {
    Idle,
    Pending,
    Found(renodx::Mod),
    NotFound,
    Failed(String),
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
            renodx_on: false,
            renodx: RenodxLookup::Idle,
            renodx_rx: None,
            renodx_for: None,
            skipped_version: cc
                .storage
                .and_then(|s| s.get_string("skip_version"))
                .unwrap_or_default(),
        };
        app.refresh();
        app.start_update_check();
        app
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
        if self.resolved_exe != self.renodx_for {
            self.renodx_for = self.resolved_exe.clone();
            self.renodx_on = false;
            self.start_renodx_lookup();
        }
    }

    fn start_renodx_lookup(&mut self) {
        let Some(exe) = self.exe() else {
            self.renodx = RenodxLookup::Idle;
            return;
        };
        let (tx, rx) = channel::<RenodxLookup>();
        self.renodx_rx = Some(rx);
        self.renodx = RenodxLookup::Pending;
        thread::spawn(move || {
            let r = match net::client().and_then(|c| renodx::lookup(&c, &exe)) {
                Ok(Some(m)) => RenodxLookup::Found(m),
                Ok(None) => RenodxLookup::NotFound,
                Err(e) => RenodxLookup::Failed(format!("{e:#}")),
            };
            let _ = tx.send(r);
        });
    }

    fn pump_renodx(&mut self) {
        let Some(rx) = &self.renodx_rx else { return };
        if let Ok(r) = rx.try_recv() {
            self.renodx = r;
            self.renodx_rx = None;
        }
    }

    fn start(&mut self, remove: Option<bool>) {
        let Some(exe) = self.exe() else { return };
        let engine = self.engine;
        let with_renodx = self.renodx_on;
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
                    with_renodx,
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
        match diagnose::run(&exe) {
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
                }
                Err(e) => {
                    self.progress_msg = "Failed.".into();
                    self.log.push(LogLine::Fail(e.clone()));
                    self.last_error = Some(e);
                }
            }
            self.refresh();
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

const TILE_RENODX: Tile = Tile {
    title: "RenoDX HDR mod",
    detail: "game-specific renodx-*.addon64 · Home → Add-ons → RenoDX",
    ok: |s| s.renodx_mod.is_some(),
    optional: true,
};

const TILE_REFRAMEWORK: Tile = Tile {
    title: "REFramework",
    detail: "dinput8.dll · RE Engine games need it before ReShade",
    ok: |s| s.reframework,
    optional: false,
};

fn tiles_for(st: Option<&GameStatus>, engine: Engine, renodx_on: bool) -> Vec<&'static Tile> {
    let mut v = base_tiles(st, engine);
    if st.is_some_and(|s| s.re_engine) {
        v.insert(0, &TILE_REFRAMEWORK);
    }
    if renodx_on || st.is_some_and(|s| s.renodx_mod.is_some()) {
        v.push(&TILE_RENODX);
    }
    v
}

fn base_tiles(st: Option<&GameStatus>, engine: Engine) -> Vec<&'static Tile> {
    match st.map(|s| s.mode) {
        Some(game::Mode::Native) if engine == Engine::Opti || st.is_some_and(|s| s.opti) => {
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

/// One selectable engine card: painted like a component tile, but clickable,
/// with a radio dot, an accent border when chosen and a dimmed body when the
/// game cannot use it.
#[allow(clippy::too_many_arguments)]
fn engine_card(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    selected: bool,
    enabled: bool,
    title: &str,
    lines: &[&str],
    note: &str,
) -> bool {
    let resp = ui.allocate_rect(rect, egui::Sense::click());
    let hovered = enabled && resp.hovered();
    let fill = if selected {
        Color32::from_rgb(0x22, 0x27, 0x30)
    } else if hovered {
        Color32::from_rgb(0x1d, 0x21, 0x29)
    } else {
        t::TILE
    };
    let border = if selected {
        t::ACCENT
    } else if enabled {
        t::BORDER_STRONG
    } else {
        t::BORDER
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(if selected { 2.0 } else { 1.0 }, border),
        StrokeKind::Inside,
    );

    // radio dot
    let c = egui::pos2(rect.left() + 20.0, rect.top() + 22.0);
    if selected {
        painter.circle_filled(c, 8.0, t::ACCENT);
        painter.circle_filled(c, 3.4, t::BG);
    } else {
        painter.circle_stroke(
            c,
            7.5,
            Stroke::new(1.6, if enabled { t::TEXT_DIM } else { t::RING_OFF }),
        );
    }

    let title_color = if !enabled {
        t::TEXT_DIM
    } else if selected {
        t::TEXT
    } else {
        t::TEXT_SOFT
    };
    let inner = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 38.0, rect.top() + 10.0),
        egui::pos2(rect.right() - 12.0, rect.bottom() - 8.0),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(title)
                    .font(t::plex_semibold(13.5))
                    .color(title_color),
            );
            for l in lines {
                ui.label(RichText::new(*l).font(t::plex(11.0)).color(if enabled {
                    t::TEXT_MUTED
                } else {
                    t::TEXT_DIM
                }));
            }
            if !note.is_empty() {
                ui.label(RichText::new(note).font(t::plex(11.0)).color(t::TEXT_DIM));
            }
        },
    );
    enabled && resp.clicked()
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
        self.pump_renodx();
        if self.running || matches!(self.renodx, RenodxLookup::Pending) {
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
                if let Some(ac) = ok_status.as_ref().and_then(|s| s.anticheat) {
                    let mut on = game::ignore_anticheat();
                    let label = format!(
                        "{ac} is switched off for offline play in this game (GTA V: BattlEye unticked in the Rockstar Games Launcher, or -nobattleye) — install anyway, at my own risk"
                    );
                    let cb = egui::Checkbox::new(&mut on, RichText::new(label).font(t::plex(11.5)).color(t::TEXT_SOFT));
                    if ui.add_enabled(!self.running, cb).changed() {
                        game::set_ignore_anticheat(on);
                        self.inspect_resolved();
                    }
                }

                // ── engine chooser ───────────────────────────────
                let native = ok_status.as_ref().is_some_and(|s| s.mode == game::Mode::Native);
                if !native {
                    self.engine = Engine::ReShade;
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(
                        RichText::new("INSTALL ENGINE")
                            .font(t::plex_semibold(11.0))
                            .color(t::TEXT_MUTED),
                    );
                    ui.label(
                        RichText::new(if native {
                            "— two ways to run DLSS 5 in this game, pick one"
                        } else {
                            "— this game has no DLSS of its own, so only the ReShade path can work"
                        })
                        .font(t::plex(11.0))
                        .color(t::TEXT_DIM),
                    );
                });
                {
                    let gap = 8.0;
                    let card_h = 74.0;
                    let row_w = ui.available_width();
                    let col_w = ((row_w - gap) / 2.0).floor();
                    let (row_rect, _) =
                        ui.allocate_exact_size(Vec2::new(row_w, card_h), egui::Sense::hover());
                    let left = egui::Rect::from_min_size(row_rect.min, Vec2::new(col_w, card_h));
                    let right = egui::Rect::from_min_size(
                        egui::pos2(row_rect.left() + col_w + gap, row_rect.top()),
                        Vec2::new(col_w, card_h),
                    );
                    if engine_card(
                        ui,
                        left,
                        self.engine == Engine::ReShade,
                        true,
                        "ReShade + DLSS 5 add-on",
                        &["The default. Works in every supported game.", "In game: Home → Add-ons → DLSS 5 Neural Rendering."],
                        "",
                    ) {
                        self.engine = Engine::ReShade;
                    }
                    if engine_card(
                        ui,
                        right,
                        self.engine == Engine::Opti,
                        native,
                        "OptiScaler (built-in NR pass)",
                        &["Dagherbou's fork, no ReShade. Also swaps upscalers.", "In game: Insert → enable Neural Rendering."],
                        if native {
                            ""
                        } else {
                            "Needs a game with its own DLSS — this one has none."
                        },
                    ) {
                        self.engine = Engine::Opti;
                    }
                }
                ui.add_space(2.0);

                // ── tiles ─────────────────────────────────────────
                let gap = 8.0;
                let tile_h = 58.0;
                let row_w = ui.available_width();
                let col_w = ((row_w - gap) / 2.0).floor();
                let tiles = tiles_for(ok_status.as_ref(), self.engine, self.renodx_on);
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

                // ── RenoDX HDR mod ────────────────────────────────
                if let Some(s) = &ok_status {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            RichText::new("RENODX HDR MOD")
                                .font(t::plex_semibold(11.0))
                                .color(t::TEXT_MUTED),
                        );
                        let dim = |ui: &mut egui::Ui, text: String| {
                            ui.label(RichText::new(text).font(t::plex(11.0)).color(t::TEXT_DIM));
                        };
                        if let Some(installed) = &s.renodx_mod {
                            dim(ui, format!("— installed: {installed} (Remove takes it out too)"));
                        } else if !s.foreign_renodx.is_empty() {
                            dim(ui, format!(
                                "— already present, not installed by this tool: {} (left untouched; ReShade loads one RenoDX mod per game)",
                                s.foreign_renodx.join(", ")
                            ));
                        } else {
                            match &self.renodx {
                                RenodxLookup::Idle | RenodxLookup::Pending => dim(ui, "— looking up clshortfuse/renodx for this game…".into()),
                                RenodxLookup::NotFound => dim(ui, "— no RenoDX mod is published for this game.".into()),
                                RenodxLookup::Failed(e) => dim(ui, format!("— lookup failed: {e}")),
                                RenodxLookup::Found(m) => {
                                    let label = format!("Also install {} — {}", m.file, m.status_label());
                                    let cb = egui::Checkbox::new(&mut self.renodx_on, RichText::new(label).font(t::plex(12.0)).color(t::TEXT_SOFT));
                                    ui.add_enabled(!self.running, cb).on_hover_text(
                                        "Game-specific HDR / tone-mapping mod from the RenoDX project. Loads beside the DLSS 5 add-on (different add-on name, different settings section). Turn Windows AutoHDR / RTX HDR off to avoid double tone mapping.",
                                    );
                                    if self.engine == Engine::Opti {
                                        dim(ui, "— ReShade goes in as ReShade64.dll, loaded by OptiScaler (LoadReshade=true)".into());
                                    }
                                    if !m.note.is_empty() {
                                        dim(ui, format!("— {}", m.note));
                                    }
                                }
                            }
                        }
                    });
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
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([720.0, 660.0])
        .with_min_inner_size([700.0, 600.0])
        .with_title(concat!("DLSS5oneclick ", env!("CARGO_PKG_VERSION")));
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
