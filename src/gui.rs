//! egui window, "instrument panel" layout: header with mark, path row, component
//! tiles, one Install button, progress, log.

use crate::game::{self, GameStatus};
use crate::installer::{self, StepState};
use crate::logo;
use crate::theme::{self as t};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Stroke, StrokeKind, Vec2,
};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

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
        };
        app.refresh();
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
    }

    fn start(&mut self, remove: bool) {
        let Some(exe) = self.exe() else { return };
        let (tx, rx): (Sender<Msg>, Receiver<Msg>) = channel();
        self.rx = Some(rx);
        self.running = true;
        self.progress = 0;
        self.progress_msg.clear();
        self.log.clear();
        self.last_error = None;
        thread::spawn(move || {
            let out = if remove {
                installer::uninstall(&exe)
                    .map(|r| {
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
                installer::run_all(
                    &exe,
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
                .map(|_| "Done. In game: Home → DLSS 5 Neural Rendering → enable.".to_owned())
                .map_err(|e| format!("{e:#}"))
            };
            let _ = tx.send(Msg::Finished(out));
        });
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
        detail: "optional · driver copy used when absent",
        ok: |s| s.dlss,
        optional: true,
    },
];

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
        detail: "dlss5-dx11-bridge.addon64 · only for D3D11 games",
        ok: |s| s.bridge,
        optional: true,
    },
];

fn tiles_for(st: Option<&GameStatus>) -> Vec<&'static Tile> {
    match st.map(|s| s.mode) {
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
            .frame(Frame::new().fill(t::HEADER).inner_margin(Margin { left: 18, right: 28, top: 12, bottom: 12 }).stroke(Stroke::new(1.0, t::BORDER)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), egui::Sense::hover());
                    logo::paint_mark(ui.painter(), rect, t::ACCENT, t::BG);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new("DLSS5oneclick").font(t::sora(15.0)).color(t::TEXT));
                        ui.label(RichText::new("Sets up the leaked DLSS 5 neural-rendering build in any DX11/DX12 game, with or without DLSS")
                            .font(t::plex(11.0)).color(t::TEXT_MUTED));
                    });
                    chip(ui, "LEAKED BUILD", t::ACCENT, true);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        chip(ui, concat!("v", env!("CARGO_PKG_VERSION")), t::TEXT_DIM, false);
                    });
                });
            });

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
                                RichText::new(format!("{}-bit · {} · {mode}", s.bitness, s.api.label()))
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
                        self.start(false);
                    }
                    let remove = egui::Button::new(RichText::new("Remove").font(t::plex_medium(13.0)).color(t::TEXT_OFF))
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0, t::BORDER_STRONG))
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(90.0, 42.0));
                    if ui.add_enabled(ok_status.is_some() && !self.running, remove).clicked() {
                        self.confirm_remove = true;
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
                    ui.label("Remove DLSS5-Feeder, LumeniteFX and the DLSS 5 add-on from this game?\nReShade itself and nvngx_dlss.dll stay.");
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() { self.confirm_remove = false; self.start(true); }
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
        .with_inner_size([700.0, 560.0])
        .with_min_inner_size([620.0, 480.0])
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
