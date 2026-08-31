//! egui window: pick game exe, see what's present, one Install button.

use crate::game::{self, GameStatus};
use crate::installer::{self, StepState, STEPS};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

enum Msg {
    Progress(u8, String),
    Log(String),
    Finished(Result<String, String>),
}

pub struct App {
    exe_text: String,
    status: Option<Result<GameStatus, String>>,
    running: bool,
    progress: u8,
    progress_msg: String,
    log: Vec<String>,
    rx: Option<Receiver<Msg>>,
    confirm_remove: bool,
    last_error: Option<String>,
    candidates: Vec<PathBuf>,
    resolved_exe: Option<PathBuf>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
        self.status = self.resolved_exe.as_ref().map(|p| game::inspect(p).map_err(|e| format!("{e:#}")));
    }

    fn start(&mut self, remove: bool) {
        let Some(exe) = self.exe() else { return };
        let (tx, rx): (Sender<Msg>, Receiver<Msg>) = channel();
        self.rx = Some(rx);
        self.running = true;
        self.progress = 0;
        self.log.clear();
        self.last_error = None;
        thread::spawn(move || {
            let out = if remove {
                installer::uninstall(&exe)
                    .map(|r| format!("Removed:\n{}", if r.is_empty() { "nothing".into() } else { r.join("\n") }))
                    .map_err(|e| format!("{e:#}"))
            } else {
                let n = STEPS.len();
                let p_tx = tx.clone();
                let s_tx = tx.clone();
                installer::run_all(
                    &exe,
                    &move |pct, msg| { let _ = p_tx.send(Msg::Progress(pct, msg.to_owned())); },
                    &move |i, name, state, detail| {
                        let line = match state {
                            StepState::Start => format!("[{}/{n}] {name}…", i + 1),
                            StepState::Done => format!("      ok: {detail}"),
                            StepState::Error => format!("      FAILED: {detail}"),
                        };
                        let _ = s_tx.send(Msg::Log(line));
                    },
                )
                .map(|_| "Done. In game: Home -> DLSS 5 Neural Rendering panel -> enable.".to_owned())
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
                Msg::Progress(p, s) => { self.progress = p; self.progress_msg = s; }
                Msg::Log(l) => self.log.push(l),
                Msg::Finished(r) => finished = Some(r),
            }
        }
        if let Some(r) = finished {
            self.rx = None;
            self.running = false;
            match r {
                Ok(msg) => { self.progress = 100; self.progress_msg = "Done.".into(); self.log.push(msg); }
                Err(e) => { self.progress_msg = "Failed.".into(); self.log.push(e.clone()); self.last_error = Some(e); }
            }
            self.refresh();
        }
    }
}

type RowFn = fn(&GameStatus) -> bool;
const ROWS: [(&str, RowFn); 7] = [
    ("ReShade (add-on build) as dxgi.dll", |s| s.reshade),
    ("ReShade shader headers (ReShade.fxh …)", |s| s.headers),
    ("DLSS5-Feeder add-on + DLSS5_Feed.fx", |s| s.feeder),
    ("LumeniteFX motion vectors", |s| s.lumenite),
    ("renodx-dlss5.addon64", |s| s.dlss5_addon),
    ("nvngx_dlssnr.dll (DLSS 5 model)", |s| s.dlssnr),
    ("nvngx_dlss.dll", |s| s.dlss),
];

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("exe", self.exe_text.clone());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        if self.running {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.exe_text)
                        .hint_text("Game folder or the game's .exe")
                        .desired_width(f32::INFINITY),
                );
                if r.changed() {
                    self.refresh();
                }
                let start_dir = self.exe().and_then(|p| p.parent().map(|d| d.to_path_buf()));
                if ui.button("Game folder…").clicked() {
                    let mut dlg = rfd::FileDialog::new().set_title("Pick the game's install folder");
                    if let Some(d) = &start_dir {
                        dlg = dlg.set_directory(d);
                    }
                    if let Some(p) = dlg.pick_folder() {
                        self.exe_text = p.to_string_lossy().into_owned();
                        self.refresh();
                    }
                }
                if ui.button("Exe…").clicked() {
                    let mut dlg = rfd::FileDialog::new().add_filter("Executables", &["exe"]);
                    if let Some(d) = &start_dir {
                        dlg = dlg.set_directory(d);
                    }
                    if let Some(p) = dlg.pick_file() {
                        self.exe_text = p.to_string_lossy().into_owned();
                        self.refresh();
                    }
                }
            });
            if let Some(exe) = self.resolved_exe.clone() {
                let short = |p: &PathBuf| -> String {
                    p.strip_prefix(self.input_path())
                        .map(|r| r.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                };
                if self.candidates.len() > 1 {
                    let mut pick = exe.clone();
                    ui.horizontal(|ui| {
                        ui.label("Game exe:");
                        egui::ComboBox::from_id_salt("exe_pick")
                            .selected_text(short(&pick))
                            .show_ui(ui, |ui| {
                                for c in &self.candidates {
                                    ui.selectable_value(&mut pick, c.clone(), short(c));
                                }
                            });
                    });
                    if pick != exe {
                        self.resolved_exe = Some(pick);
                        self.inspect_resolved();
                    }
                } else if !self.candidates.is_empty() {
                    ui.small(format!("Game exe: {}", short(&exe)));
                }
            }
            ui.add_space(6.0);

            let (ok_status, problems, complete) = match &self.status {
                Some(Ok(s)) => (Some(s.clone()), s.problems.clone(), s.complete()),
                Some(Err(e)) => (None, vec![e.clone()], false),
                None => (None, vec![], false),
            };
            for (label, get) in ROWS {
                let mark = match &ok_status { Some(s) if get(s) => "✔", _ => "○" };
                ui.label(format!("{mark}  {label}"));
            }
            for p in &problems {
                ui.colored_label(egui::Color32::from_rgb(192, 57, 43), p);
            }
            ui.add_space(6.0);

            let can_run = ok_status.is_some() && problems.is_empty() && !self.running;
            ui.horizontal(|ui| {
                if ui.add_enabled(can_run, egui::Button::new("Install DLSS 5").min_size([200.0, 28.0].into())).clicked() {
                    self.start(false);
                }
                if ui.add_enabled(ok_status.is_some() && !self.running, egui::Button::new("Remove")).clicked() {
                    self.confirm_remove = true;
                }
            });
            ui.add(egui::ProgressBar::new(self.progress as f32 / 100.0).show_percentage());
            ui.label(if complete && !self.running && self.progress_msg.is_empty() {
                "Everything is in place."
            } else {
                self.progress_msg.as_str()
            });
            ui.add_space(4.0);

            egui::ScrollArea::vertical().stick_to_bottom(true).max_height(220.0).show(ui, |ui| {
                for l in &self.log {
                    ui.monospace(l);
                }
            });
            ui.add_space(6.0);
            ui.small(
                "After install, in game: press Home for the ReShade overlay, open the DLSS 5 Neural Rendering \
                 panel and enable it. Keep MSAA/SSAA off. Check dlss5-feed.log next to the exe for 'feature ready'.",
            );
        });

        if self.confirm_remove {
            egui::Window::new("Remove DLSS 5 files")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.label("Remove DLSS5-Feeder, LumeniteFX and the DLSS 5 add-on from this game?\nReShade itself and nvngx_dlss.dll stay.");
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() {
                            self.confirm_remove = false;
                            self.start(true);
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_remove = false;
                        }
                    });
                });
        }
        if let Some(err) = self.last_error.clone() {
            egui::Window::new("DLSS5oneclick")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.label(&err);
                    if ui.button("OK").clicked() {
                        self.last_error = None;
                    }
                });
        }
    }
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([680.0, 520.0])
            .with_min_inner_size([560.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        concat!("DLSS5oneclick ", env!("CARGO_PKG_VERSION")),
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
