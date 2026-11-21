use crate::bridge::{Bridge, Pending};
use crate::downloads::{self, Downloads};
use crate::export;
use crate::images::Images;
use crate::model::*;
use crate::reader::ReaderState;
use crate::store::{self, ReaderMode, Store};
use crate::widgets::*;
use egui::{Color32, Key, RichText, Ui};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Library,
    Books,
    History,
    Browse,
    Search,
    Downloads,
    Settings,
}

pub struct CatalogState {
    pub source: String,
    pub title: String,
    info: Option<SourceInfo>,
    info_p: Option<Pending>,
    order: Option<String>,
    query: String,
    active_query: String,
    items: Vec<Manga>,
    seen: HashSet<i64>,
    pending: Option<Pending>,
    end: bool,
    error: Option<String>,
}

impl CatalogState {
    pub fn new(source: &Source) -> Self {
        CatalogState {
            source: source.name.clone(),
            title: source.title.clone(),
            info: None,
            info_p: None,
            order: None,
            query: String::new(),
            active_query: String::new(),
            items: vec![],
            seen: HashSet::new(),
            pending: None,
            end: false,
            error: None,
        }
    }
    fn reset(&mut self) {
        self.items.clear();
        self.seen.clear();
        self.end = false;
        self.error = None;
        self.pending = None;
    }
}

pub struct SearchState {
    query: String,
    results: Vec<(String, String, Pending)>,
}

pub struct DetailsState {
    pub manga: Manga,
    pending: Option<Pending>,
    error: Option<String>,
    reverse: bool,
    filter: String,
    branch: Option<Option<String>>,
    continue_on_load: bool,
}

impl DetailsState {
    pub fn new(manga: Manga, continue_on_load: bool) -> Self {
        DetailsState { manga, pending: None, error: None, reverse: true, filter: String::new(), branch: None, continue_on_load }
    }
}

pub enum View {
    Library,
    History,
    Browse,
    Search(SearchState),
    Downloads,
    Settings,
    Catalog(CatalogState),
    Details(DetailsState),
    Reader(ReaderState),
    Books,
    Book(crate::books::BookReader),
}

pub enum Nav {
    None,
    Push(View),
    Back,
    Replace(View),
}

pub struct App {
    pub bridge: Arc<Bridge>,
    pub images: Images,
    pub store: Store,
    pub downloads: Downloads,
    pub shelf: crate::books::Shelf,
    sources: Vec<Source>,
    sources_p: Option<Pending>,
    source_filter: String,
    source_lang: String,
    stack: Vec<View>,
    tab: Tab,
    pub toast: Option<(String, Instant)>,
    updates: Vec<(String, Pending)>,
    lib_filter: String,
    applied_scale: f32,
    sync_job: Option<crossbeam_channel::Receiver<crate::sync::Outcome>>,
    login_job: Option<crossbeam_channel::Receiver<Result<String, String>>>,
    sync_password: String,
    sync_status: String,
    last_sync_try: Option<Instant>,
    /// KD_AUTOTEST=SOURCE: open that source, its first manga, and read it (for smoke tests)
    autotest: Option<(String, u8)>,
    export_dialog: Option<ExportDialog>,
    export_job: Option<export::Running>,
}

/// "Export" dialog for one manga's downloaded chapters (reading order).
pub struct ExportDialog {
    manga: Manga,
    downloaded: Vec<Chapter>,
    unread: Vec<Chapter>,
    only_unread: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> App {
        let ctx = cc.egui_ctx.clone();
        setup_style(&ctx);
        let store = Store::load();
        let rctx = ctx.clone();
        let bridge = Bridge::start(store::data_dir(), move || rctx.request_repaint());
        let images = Images::new(bridge.clone(), ctx.clone(), store.settings.texture_budget_mb);
        let downloads = Downloads::new(bridge.clone(), ctx.clone());
        let sources_p = Some(bridge.call("sources", json!({})));
        let shelf = crate::books::Shelf::new(store.book_cache.clone());
        App {
            shelf,
            bridge,
            images,
            store,
            downloads,
            sources: vec![],
            sources_p,
            source_filter: String::new(),
            source_lang: String::new(),
            stack: vec![View::Library],
            tab: Tab::Library,
            toast: None,
            updates: vec![],
            lib_filter: String::new(),
            applied_scale: 0.0,
            sync_job: None,
            login_job: None,
            sync_password: String::new(),
            sync_status: String::new(),
            last_sync_try: None,
            autotest: std::env::var("KD_AUTOTEST").ok().map(|s| (s, 0)),
            export_dialog: None,
            export_job: None,
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        if tab == Tab::Books {
            self.rescan_books();
        }
        self.stack.clear();
        self.stack.push(match tab {
            Tab::Library => View::Library,
            Tab::Books => View::Books,
            Tab::History => View::History,
            Tab::Browse => View::Browse,
            Tab::Search => View::Search(SearchState { query: String::new(), results: vec![] }),
            Tab::Downloads => View::Downloads,
            Tab::Settings => View::Settings,
        });
    }

    pub fn source_title(&self, name: &str) -> String {
        self.sources.iter().find(|s| s.name == name).map(|s| s.title.clone()).unwrap_or_else(|| name.to_string())
    }

    fn poll_background(&mut self) {
        if let Some(p) = &self.sources_p {
            if let Some(r) = p.get::<Vec<Source>>() {
                match r {
                    Ok(s) => self.sources = s,
                    Err(e) => self.toast(format!("Failed to load sources: {e}")),
                }
                self.sources_p = None;
            }
        }
        // Export progress
        if let Some(job) = &mut self.export_job {
            let mut done = None;
            while let Ok(msg) = job.rx.try_recv() {
                match msg {
                    export::Msg::Progress(d, n) => job.progress = (d, n),
                    export::Msg::Done(r) => done = Some(r),
                }
            }
            if let Some(r) = done {
                self.export_job = None;
                match r {
                    Ok(p) => self.toast(format!("Exported {}", p.display())),
                    Err(e) => self.toast(format!("Export failed: {e}")),
                }
            }
        }
        // Library update check results
        let mut i = 0;
        while i < self.updates.len() {
            if let Some(r) = self.updates[i].1.get::<Manga>() {
                let (key, _) = self.updates.swap_remove(i);
                if let Ok(m) = r {
                    let n = m.chapters.as_ref().map_or(0, Vec::len);
                    if let Some(f) = self.store.favourites.iter_mut().find(|f| f.manga.key() == key) {
                        if f.known_chapters == 0 {
                            f.known_chapters = n;
                        }
                        f.new_chapters = n.saturating_sub(f.known_chapters);
                        f.manga = m.slim();
                    }
                    self.store.touch();
                }
                if self.updates.is_empty() {
                    let total: usize = self.store.favourites.iter().map(|f| f.new_chapters).sum();
                    self.toast(format!("Update check done: {total} new chapters"));
                }
            } else {
                i += 1;
            }
        }
    }
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // CJK fallback so Japanese/Korean/Chinese titles render
    for (name, path) in [
        ("cjk", "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"),
        ("cjk", "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
        ("cjk", "/usr/share/fonts/adobe-source-han-sans/SourceHanSans.ttc"),
        ("cjk", "/usr/share/fonts/wenquanyi/wqy-zenhei/wqy-zenhei.ttc"),
    ] {
        if let Ok(data) = std::fs::read(path) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(data)));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(name.into());
            }
            break;
        }
    }
    crate::books::add_fonts(&mut fonts);
    ctx.set_fonts(fonts);
    ctx.set_visuals(egui::Visuals::dark());
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.visuals.panel_fill = Color32::from_gray(20);
        s.visuals.window_fill = Color32::from_gray(26);
        s.visuals.extreme_bg_color = Color32::from_gray(14);
        s.visuals.selection.bg_fill = Color32::from_rgb(196, 70, 60);
    });
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.applied_scale != self.store.settings.ui_scale {
            self.applied_scale = self.store.settings.ui_scale;
            ctx.set_zoom_factor(self.applied_scale);
        }
        self.poll_background();
        self.poll_sync();
        self.poll_books(&ctx);
        self.autotest_step();

        let in_reader = matches!(self.stack.last(), Some(View::Reader(_) | View::Book(_)));
        if !in_reader {
            self.top_bar(ui);
        }

        // Global back navigation
        let typing = ctx.memory(|m| m.focused().is_some());
        let back = self.export_dialog.is_none() && ctx.input(|i| {
            i.pointer.button_pressed(egui::PointerButton::Extra1)
                || (!typing && (i.key_pressed(Key::Escape) || i.key_pressed(Key::Backspace)))
                || (i.modifiers.alt && i.key_pressed(Key::ArrowLeft))
        });

        let mut view = self.stack.pop().unwrap_or(View::Library);
        let nav = egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.global_style()).inner_margin(if in_reader { 0 } else { 12 }))
            .show(ui, |ui| match &mut view {
                View::Library => self.library_ui(ui),
                View::History => self.history_ui(ui),
                View::Browse => self.sources_ui(ui),
                View::Search(s) => self.search_ui(ui, s),
                View::Downloads => self.downloads_ui(ui),
                View::Settings => self.settings_ui(ui),
                View::Catalog(s) => self.catalog_ui(ui, s),
                View::Details(s) => self.details_ui(ui, s),
                View::Reader(s) => self.reader_ui(ui, s),
                View::Books => self.books_ui(ui),
                View::Book(s) => self.book_reader_ui(ui, s),
            })
            .inner;
        if let View::Reader(r) = &mut view {
            r.on_leave_check(&mut self.store);
            // Leaving the reader: push progress to the phone soon
            if !matches!(nav, Nav::None) && self.last_sync_try.is_some_and(|t| t.elapsed().as_secs() > 30) {
                self.start_sync();
            }
        }
        match nav {
            Nav::None => {
                self.stack.push(view);
                if back && self.stack.len() > 1 && !matches!(self.stack.last(), Some(View::Reader(_) | View::Book(_))) {
                    self.stack.pop();
                }
            }
            Nav::Push(v) => {
                self.stack.push(view);
                self.stack.push(v);
            }
            Nav::Back => {
                if self.stack.is_empty() {
                    self.stack.push(view);
                }
            }
            Nav::Replace(v) => self.stack.push(v),
        }

        self.export_dialog_ui(&ctx);

        if let Some((msg, t)) = &self.toast {
            let age = t.elapsed().as_secs_f32();
            if age > 3.5 {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::unique("toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(msg.as_str()));
                    });
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }

        self.images.end_frame(&ctx);
        self.store.save_if_dirty();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.store.save_if_dirty();
    }
}

impl App {
    pub fn start_sync(&mut self) {
        if self.sync_job.is_some() {
            return;
        }
        let Some(token) = self.store.sync.token.clone() else { return };
        self.last_sync_try = Some(Instant::now());
        let snap = crate::sync::Snapshot {
            url: self.store.sync.url.clone(),
            token,
            favourites: self.store.favourites.clone(),
            history: self.store.history.clone(),
            fav_removed: self.store.fav_removed.clone(),
            hist_removed: self.store.hist_removed.clone(),
            remote_manga: self.store.remote_manga.clone(),
        };
        let (tx, rx) = crossbeam_channel::bounded(1);
        let bridge = self.bridge.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::sync::run(&bridge, snap));
            bridge.repaint();
        });
        self.sync_job = Some(rx);
        self.sync_status = "Syncing…".into();
    }

    fn poll_sync(&mut self) {
        // Auto sync: at start, then every 10 minutes
        if self.store.sync.auto
            && self.store.sync.token.is_some()
            && self.sources_p.is_none()
            && self.last_sync_try.is_none_or(|t| t.elapsed().as_secs() > 600)
        {
            self.start_sync();
        }
        if let Some(out) = self.sync_job.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.sync_job = None;
            let (mut added, mut removed, mut progress) = (0, 0, 0);
            for op in out.ops {
                match op {
                    crate::sync::Op::AddFav(m, added_ms) => {
                        if !self.store.is_favourite(&m) {
                            self.store.favourites.push(store::Favourite { manga: m, added: added_ms / 1000, known_chapters: 0, new_chapters: 0 });
                            added += 1;
                        }
                    }
                    crate::sync::Op::RemoveFav(key) => {
                        self.store.favourites.retain(|f| f.manga.key() != key);
                        removed += 1;
                    }
                    crate::sync::Op::SetHistory(h) => {
                        let key = h.manga.key();
                        self.store.history.retain(|x| x.manga.key() != key);
                        self.store.history.push(h);
                        progress += 1;
                    }
                    crate::sync::Op::RemoveHistory(key) => self.store.history.retain(|x| x.manga.key() != key),
                }
            }
            self.store.history.sort_by(|a, b| b.at.cmp(&a.at));
            self.store.favourites.sort_by(|a, b| b.added.cmp(&a.added));
            self.store.remote_manga.extend(out.remote_manga);
            // Tombstones only need to outlive a few syncs
            let cutoff = (store::now() - 30 * 86400) * 1000;
            self.store.fav_removed.retain(|_, t| *t > cutoff);
            self.store.hist_removed.retain(|_, t| *t > cutoff);
            if out.unauthorized {
                self.store.sync.token = None;
            }
            match out.error {
                Some(e) => {
                    self.sync_status = format!("Sync failed: {e}");
                    self.toast(self.sync_status.clone());
                }
                None => {
                    self.store.sync.last_sync = store::now();
                    self.sync_status = format!("Synced: +{added} / -{removed} library, {progress} progress updates, {} sent", out.sent);
                    if added + removed + progress > 0 {
                        self.toast(self.sync_status.clone());
                    }
                }
            }
            self.store.touch();
        }
        if let Some(r) = self.login_job.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.login_job = None;
            match r {
                Ok(token) => {
                    self.store.sync.token = Some(token);
                    self.store.touch();
                    self.sync_status = "Logged in".into();
                    self.start_sync();
                }
                Err(e) => self.sync_status = format!("Login failed: {e}"),
            }
        }
    }

    fn sync_settings_ui(&mut self, ui: &mut Ui) {
        ui.heading("Phone sync");
        ui.label(RichText::new("Uses the same sync account as Kotatsu on your phone (phone: Settings → Services → Synchronization). Syncs library and reading progress.").small().weak());
        let logged_in = self.store.sync.token.is_some();
        egui::Grid::new("sync").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
            ui.label("Server");
            ui.add_enabled(!logged_in, egui::TextEdit::singleline(&mut self.store.sync.url).desired_width(280.0));
            ui.end_row();
            ui.label("Email");
            ui.add_enabled(!logged_in, egui::TextEdit::singleline(&mut self.store.sync.email).desired_width(280.0));
            ui.end_row();
            if !logged_in {
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(&mut self.sync_password).password(true).desired_width(280.0));
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            if logged_in {
                if ui.add_enabled(self.sync_job.is_none(), egui::Button::new("Sync now")).clicked() {
                    self.start_sync();
                }
                if ui.checkbox(&mut self.store.sync.auto, "Sync automatically").changed() {
                    self.store.touch();
                }
                if ui.button("Log out").clicked() {
                    self.store.sync.token = None;
                    self.store.touch();
                    self.sync_status = "Logged out".into();
                }
            } else {
                let can = self.login_job.is_none() && !self.store.sync.email.is_empty() && !self.sync_password.is_empty();
                if ui.add_enabled(can, egui::Button::new("Log in")).clicked() {
                    let (tx, rx) = crossbeam_channel::bounded(1);
                    let (bridge, url, email, pw) = (self.bridge.clone(), self.store.sync.url.clone(), self.store.sync.email.clone(), std::mem::take(&mut self.sync_password));
                    std::thread::spawn(move || {
                        let _ = tx.send(crate::sync::login(&bridge, &url, &email, &pw));
                        bridge.repaint();
                    });
                    self.login_job = Some(rx);
                    self.sync_status = "Logging in…".into();
                    self.store.touch();
                }
            }
            if self.sync_job.is_some() || self.login_job.is_some() {
                ui.spinner();
            }
        });
        if !self.sync_status.is_empty() {
            ui.label(RichText::new(&self.sync_status).small());
        }
        if self.store.sync.last_sync > 0 {
            ui.label(RichText::new(format!("Last synced {}", time_ago(self.store.sync.last_sync))).small().weak());
        }
    }

    fn autotest_step(&mut self) {
        let Some((src, step)) = self.autotest.clone() else { return };
        if let Some(path) = src.strip_prefix("BOOK:") {
            self.stack.push(View::Book(crate::books::BookReader::new(path.into())));
            self.autotest = None;
            return;
        }
        // EXPORT: open the first downloaded manga and its export dialog
        if src == "EXPORT" {
            match self.stack.last() {
                Some(View::Details(d)) if d.manga.chapters.as_ref().is_some_and(|c| !c.is_empty()) => {
                    let m = d.manga.clone();
                    self.open_export(&m, None);
                    self.autotest = None;
                }
                Some(View::Details(_)) => {}
                _ => {
                    if let Some(m) = downloads::downloaded_manga().into_iter().next() {
                        self.stack.push(View::Details(DetailsState::new(m, false)));
                    } else {
                        self.autotest = None;
                    }
                }
            }
            return;
        }
        if src == "GET" {
            self.set_tab(Tab::Books);
            self.shelf.tab = crate::books::ShelfTab::Get;
            self.autotest = None;
            return;
        }
        let next = match (step, self.stack.last()) {
            (0, _) if !self.sources.is_empty() => {
                if let Some(s) = self.sources.iter().find(|s| s.name == src).cloned() {
                    self.stack.push(View::Catalog(CatalogState::new(&s)));
                }
                1
            }
            (1, Some(View::Catalog(c))) if !c.items.is_empty() => {
                let m = c.items[0].clone();
                self.stack.push(View::Details(DetailsState::new(m, false)));
                2
            }
            (2, Some(View::Details(d))) if d.manga.chapters.as_ref().is_some_and(|c| !c.is_empty()) => {
                let m = d.manga.clone();
                let chs = m.chapters.clone().unwrap_or_default();
                let mut bs: Vec<Option<String>> = vec![];
                for c in &chs { if !bs.contains(&c.branch) { bs.push(c.branch.clone()); } }
                let b = default_branch(&bs);
                let idx = chs.iter().position(|c| c.branch == b).unwrap_or(0);
                self.stack.push(View::Reader(ReaderState::new(m, idx, 0, self.store.settings.reader_mode)));
                3
            }
            (s, _) => s,
        };
        if next != step {
            eprintln!("autotest: step {next}");
        }
        self.autotest = if next >= 3 { None } else { Some((src, next)) };
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Kotatsu").strong().size(16.0));
                ui.add_space(12.0);
                let tabs = [
                    (Tab::Library, "Library"),
                    (Tab::Books, "Books"),
                    (Tab::History, "History"),
                    (Tab::Browse, "Browse"),
                    (Tab::Search, "Search"),
                    (Tab::Downloads, "Downloads"),
                    (Tab::Settings, "Settings"),
                ];
                for (t, label) in tabs {
                    if ui.selectable_label(self.tab == t, label).clicked() {
                        self.set_tab(t);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(e) = self.bridge.error.lock().unwrap().as_ref() {
                        ui.colored_label(Color32::from_rgb(230, 100, 100), format!("⚠ {e}"));
                    }
                    if let Some((title, ch, d, t)) = &self.downloads.progress.lock().unwrap().current {
                        ui.label(RichText::new(format!("⬇ {title} {ch} {d}/{t}")).small());
                    }
                    if let Some(job) = &self.export_job {
                        if ui.small_button("✕").on_hover_text("Cancel export").clicked() {
                            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        let (d, n) = job.progress;
                        ui.add(egui::ProgressBar::new(d as f32 / n.max(1) as f32).desired_width(90.0));
                        ui.label(RichText::new(format!("📤 {} {d}/{n}", job.title)).small());
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    // ------------------------------------------------------------------ library

    fn library_ui(&mut self, ui: &mut Ui) -> Nav {
        let mut nav = Nav::None;
        ui.horizontal(|ui| {
            ui.heading("Library");
            ui.add(egui::TextEdit::singleline(&mut self.lib_filter).hint_text("Filter").desired_width(200.0));
            let busy = !self.updates.is_empty();
            if ui.add_enabled(!busy, egui::Button::new(if busy { "Checking…" } else { "Check updates" })).clicked() {
                for f in &self.store.favourites {
                    let p = self.bridge.call("details", json!({"manga": f.manga}));
                    self.updates.push((f.manga.key(), p));
                }
            }
            if busy {
                ui.spinner();
                ui.label(format!("{} left", self.updates.len()));
            }
        });
        ui.separator();
        if self.store.favourites.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label("Your library is empty. Find something in Browse or Search and press ☆.");
            });
            return nav;
        }
        let q = self.lib_filter.to_lowercase();
        let favs: Vec<(Manga, usize)> = self
            .store
            .favourites
            .iter()
            .filter(|f| q.is_empty() || f.manga.title.to_lowercase().contains(&q))
            .map(|f| (f.manga.clone(), f.new_chapters))
            .collect();
        let items: Vec<Manga> = favs.iter().map(|f| f.0.clone()).collect();
        let badge = |i: usize| favs.get(i).filter(|f| f.1 > 0).map(|f| f.1.to_string());
        let out = manga_grid(ui, &mut self.images, "library", &items, &badge);
        if let Some(i) = out.clicked {
            nav = Nav::Push(View::Details(DetailsState::new(items[i].clone(), false)));
        }
        nav
    }

    // ------------------------------------------------------------------ history

    fn history_ui(&mut self, ui: &mut Ui) -> Nav {
        let mut nav = Nav::None;
        ui.horizontal(|ui| {
            ui.heading("History");
            if !self.store.history.is_empty() && ui.small_button("Clear").clicked() {
                let t = store::now() * 1000;
                for h in std::mem::take(&mut self.store.history) {
                    self.store.hist_removed.insert(h.manga.key(), t);
                }
                self.store.touch();
            }
        });
        ui.separator();
        let row_h = 96.0;
        let n = self.store.history.len();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let h = self.store.history[i].clone();
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h - 6.0), egui::Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, 6, Color32::from_gray(32));
                }
                let cover = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), egui::vec2(58.0, row_h - 14.0));
                paint_cover(ui, &mut self.images, &h.manga, cover);
                let tx = cover.right() + 12.0;
                let p = ui.painter();
                p.text(egui::pos2(tx, rect.top() + 10.0), egui::Align2::LEFT_TOP, &h.manga.title, egui::FontId::proportional(15.0), ui.visuals().strong_text_color());
                p.text(
                    egui::pos2(tx, rect.top() + 34.0),
                    egui::Align2::LEFT_TOP,
                    if h.chapter_label.is_empty() { format!("page {} · {:.0}% read", h.page + 1, h.percent * 100.0) } else { format!("{} · page {}", h.chapter_label, h.page + 1) },
                    egui::FontId::proportional(13.0),
                    ui.visuals().text_color(),
                );
                p.text(
                    egui::pos2(tx, rect.top() + 56.0),
                    egui::Align2::LEFT_TOP,
                    format!("{} · {}", self.source_title(&h.manga.source), time_ago(h.at)),
                    egui::FontId::proportional(12.0),
                    Color32::GRAY,
                );
                if resp.clicked() {
                    nav = Nav::Push(View::Details(DetailsState::new(h.manga.clone(), true)));
                }
            }
        });
        nav
    }

    // ------------------------------------------------------------------ sources

    fn visible_sources(&self) -> Vec<&Source> {
        let q = self.source_filter.to_lowercase();
        let s = &self.store.settings;
        let mut v: Vec<&Source> = self
            .sources
            .iter()
            .filter(|x| s.show_broken || !x.broken)
            .filter(|x| s.show_adult || x.kind != "HENTAI")
            .filter(|x| self.source_lang.is_empty() || x.locale == self.source_lang)
            .filter(|x| q.is_empty() || x.title.to_lowercase().contains(&q) || x.name.to_lowercase().contains(&q))
            .collect();
        let pinned = &self.store.pinned_sources;
        v.sort_by_key(|x| (!pinned.contains(&x.name), x.title.to_lowercase()));
        v
    }

    fn sources_ui(&mut self, ui: &mut Ui) -> Nav {
        let mut nav = Nav::None;
        if self.sources_p.is_some() {
            spinner(ui);
            return nav;
        }
        let mut langs: Vec<String> = self.sources.iter().map(|s| s.locale.clone()).collect::<HashSet<_>>().into_iter().collect();
        langs.sort();
        ui.horizontal(|ui| {
            ui.heading("Sources");
            ui.add(egui::TextEdit::singleline(&mut self.source_filter).hint_text("Filter sources").desired_width(220.0));
            egui::ComboBox::from_id_salt("lang")
                .selected_text(if self.source_lang.is_empty() { "All languages".to_string() } else { self.source_lang.clone() })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.source_lang, String::new(), "All languages");
                    for l in &langs {
                        ui.selectable_value(&mut self.source_lang, l.clone(), if l.is_empty() { "multi" } else { l.as_str() });
                    }
                });
        });
        ui.separator();
        let list: Vec<Source> = self.visible_sources().into_iter().cloned().collect();
        ui.label(RichText::new(format!("{} sources", list.len())).small().weak());
        let mut toggle = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 30.0, list.len(), |ui, range| {
            for s in &list[range] {
                ui.horizontal(|ui| {
                    ui.set_height(26.0);
                    let pinned = self.store.pinned_sources.contains(&s.name);
                    if ui.button(if pinned { "★" } else { "☆" }).on_hover_text("Pin (pinned sources are used by Search)").clicked() {
                        toggle = Some(s.name.clone());
                    }
                    let label = format!("{}{}", s.title, if s.broken { "  (broken)" } else { "" });
                    if ui.add(egui::Button::new(label).frame(false)).clicked() {
                        nav = Nav::Push(View::Catalog(CatalogState::new(s)));
                    }
                    ui.label(RichText::new(format!("{} · {}", if s.locale.is_empty() { "multi" } else { &s.locale }, s.kind.to_lowercase())).small().weak());
                });
            }
        });
        if let Some(t) = toggle {
            self.store.toggle_pin(&t);
        }
        nav
    }

    // ------------------------------------------------------------------ catalog

    fn catalog_ui(&mut self, ui: &mut Ui, st: &mut CatalogState) -> Nav {
        let mut nav = Nav::None;
        if st.info.is_none() && st.info_p.is_none() {
            st.info_p = Some(self.bridge.call("info", json!({"source": st.source})));
        }
        if let Some(r) = st.info_p.as_ref().and_then(|p| p.get::<SourceInfo>()) {
            st.info = Some(r.unwrap_or_default());
            st.info_p = None;
        }
        ui.horizontal(|ui| {
            if ui.button("←").clicked() {
                nav = Nav::Back;
            }
            ui.heading(&st.title);
            if let Some(info) = st.info.clone() {
                if info.search {
                    let r = ui.add(egui::TextEdit::singleline(&mut st.query).hint_text("Search this source").desired_width(240.0));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && st.query != st.active_query {
                        st.active_query = st.query.clone();
                        st.reset();
                    }
                }
                if st.active_query.is_empty() && !info.orders.is_empty() {
                    let cur = st.order.clone().unwrap_or_else(|| {
                        if info.orders.iter().any(|o| o == "POPULARITY") { "POPULARITY".into() } else { info.orders[0].clone() }
                    });
                    let mut sel = cur.clone();
                    egui::ComboBox::from_id_salt("order").selected_text(order_label(&cur)).show_ui(ui, |ui| {
                        for o in &info.orders {
                            ui.selectable_value(&mut sel, o.clone(), order_label(o));
                        }
                    });
                    if sel != cur {
                        st.order = Some(sel);
                        st.reset();
                    }
                }
                if !info.domain.is_empty() {
                    ui.label(RichText::new(&info.domain).small().weak());
                }
            }
        });
        ui.separator();

        if let Some(r) = st.pending.as_ref().and_then(|p| p.get::<Vec<Manga>>()) {
            st.pending = None;
            match r {
                Ok(list) => {
                    let before = st.items.len();
                    let adult = self.store.settings.show_adult;
                    for m in list {
                        if (adult || !m.adult) && st.seen.insert(m.id) {
                            st.items.push(m);
                        }
                    }
                    if st.items.len() == before {
                        st.end = true;
                    }
                }
                Err(e) => st.error = Some(e),
            }
        }
        if let Some(e) = &st.error {
            if error_box(ui, e) {
                st.error = None;
            }
            if st.items.is_empty() {
                return nav;
            }
        }
        let need_more = st.items.is_empty() && !st.end;
        if st.items.is_empty() && st.end {
            ui.label("Nothing found.");
            return nav;
        }
        let out = manga_grid(ui, &mut self.images, ("catalog", &st.source), &st.items, &|_| None);
        if (need_more || out.near_end) && st.pending.is_none() && !st.end && st.error.is_none() {
            let mut p = json!({"source": st.source, "offset": st.items.len()});
            if !st.active_query.is_empty() {
                p["query"] = json!(st.active_query);
            }
            if let Some(o) = &st.order {
                p["order"] = json!(o);
            }
            st.pending = Some(self.bridge.call("list", p));
        }
        if st.pending.is_some() && st.items.is_empty() {
            spinner(ui);
        }
        if let Some(i) = out.clicked {
            nav = Nav::Push(View::Details(DetailsState::new(st.items[i].clone(), false)));
        }
        nav
    }

    // ------------------------------------------------------------------ global search

    fn search_ui(&mut self, ui: &mut Ui, st: &mut SearchState) -> Nav {
        let mut nav = Nav::None;
        ui.horizontal(|ui| {
            ui.heading("Search");
            let r = ui.add(egui::TextEdit::singleline(&mut st.query).hint_text("Title, then Enter").desired_width(320.0));
            if st.results.is_empty() && st.query.is_empty() {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !st.query.trim().is_empty() {
                for (_, _, p) in &st.results {
                    self.bridge.cancel(p);
                }
                st.results = self
                    .store
                    .pinned_sources
                    .iter()
                    .map(|s| (s.clone(), self.source_title(s), self.bridge.call("list", json!({"source": s, "offset": 0, "query": st.query.trim()}))))
                    .collect();
            }
        });
        ui.label(RichText::new(format!("Searches your {} pinned sources (pin more in Browse with ☆)", self.store.pinned_sources.len())).small().weak());
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // Sources with results first
            let mut order: Vec<usize> = (0..st.results.len()).collect();
            order.sort_by_key(|&i| match st.results[i].2.get::<Vec<Manga>>() {
                Some(Ok(v)) if !v.is_empty() => 0,
                None => 1,
                _ => 2,
            });
            for i in order {
                let (src, title, p) = &st.results[i];
                ui.horizontal(|ui| {
                    ui.strong(title);
                    match p.get::<Vec<Manga>>() {
                        None => {
                            ui.spinner();
                        }
                        Some(Err(e)) => {
                            ui.colored_label(Color32::GRAY, e.chars().take(80).collect::<String>());
                        }
                        Some(Ok(v)) => {
                            ui.label(RichText::new(format!("{} results", v.len())).weak());
                        }
                    }
                });
                if let Some(Ok(v)) = p.get::<Vec<Manga>>() {
                    if !v.is_empty() {
                        if let Some(c) = cover_row(ui, &mut self.images, ("sr", src), &v) {
                            nav = Nav::Push(View::Details(DetailsState::new(v[c].clone(), false)));
                        }
                    }
                }
                ui.add_space(8.0);
            }
        });
        nav
    }

    // ------------------------------------------------------------------ details

    fn details_ui(&mut self, ui: &mut Ui, st: &mut DetailsState) -> Nav {
        let mut nav = Nav::None;
        if st.manga.chapters.is_none() && st.pending.is_none() && st.error.is_none() {
            st.pending = Some(self.bridge.call("details", json!({"manga": st.manga})));
        }
        if let Some(r) = st.pending.as_ref().and_then(|p| p.get::<Manga>()) {
            st.pending = None;
            match r {
                Ok(m) => {
                    // Clear the "new chapters" badge once looked at
                    if let Some(f) = self.store.favourites.iter_mut().find(|f| f.manga.key() == m.key()) {
                        f.known_chapters = m.chapters.as_ref().map_or(0, Vec::len);
                        f.new_chapters = 0;
                        self.store.touch();
                    }
                    st.manga = m;
                }
                Err(e) => st.error = Some(e),
            }
        }
        let m = st.manga.clone();
        let chapters: &[Chapter] = m.chapters.as_deref().unwrap_or(&[]);

        // Continue-from-history shortcut
        let last = self.store.last_read(&m).cloned();
        if st.continue_on_load && !chapters.is_empty() {
            st.continue_on_load = false;
            if let Some(h) = &last {
                if let Some(idx) = chapters.iter().position(|c| c.id == h.chapter_id) {
                    return Nav::Push(View::Reader(ReaderState::new(m.clone(), idx, h.page, self.store.settings.reader_mode)));
                }
            }
        }

        ui.horizontal(|ui| {
            if ui.button("←").clicked() {
                nav = Nav::Back;
            }
            ui.label(RichText::new(self.source_title(&m.source)).weak());
        });
        ui.add_space(6.0);

        ui.horizontal_top(|ui| {
            let cw = 200.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(cw, cw * COVER_RATIO), egui::Sense::hover());
            paint_cover(ui, &mut self.images, &m, rect);
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(&m.title).heading().strong());
                if !m.alt.is_empty() {
                    ui.label(RichText::new(m.alt.join(" · ")).weak().small());
                }
                if !m.authors.is_empty() {
                    ui.label(m.authors.join(", "));
                }
                ui.horizontal_wrapped(|ui| {
                    if let Some(s) = &m.state {
                        ui.label(RichText::new(s.to_lowercase()).color(Color32::from_rgb(140, 190, 140)));
                    }
                    if m.rating > 0.0 {
                        ui.label(format!("★ {:.1}", m.rating * 5.0));
                    }
                    if m.adult {
                        ui.colored_label(Color32::from_rgb(220, 90, 90), "18+");
                    }
                    ui.label(RichText::new(format!("{} chapters", chapters.len())).weak());
                });
                ui.horizontal_wrapped(|ui| {
                    for t in m.tags.iter().take(20) {
                        ui.label(RichText::new(&t.title).small().background_color(Color32::from_gray(40)));
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let fav = self.store.is_favourite(&m);
                    if !chapters.is_empty() {
                        let (label, idx, page) = match &last {
                            Some(h) => match chapters.iter().position(|c| c.id == h.chapter_id) {
                                Some(i) => (format!("▶ Continue {}", chapters[i].label()), i, h.page),
                                None => ("▶ Read".into(), 0, 0),
                            },
                            None => {
                                let b = st.branch.clone().unwrap_or_else(|| {
                                    let mut bs: Vec<Option<String>> = vec![];
                                    for c in chapters { if !bs.contains(&c.branch) { bs.push(c.branch.clone()); } }
                                    default_branch(&bs)
                                });
                                ("▶ Read".into(), chapters.iter().position(|c| c.branch == b).unwrap_or(0), 0)
                            }
                        };
                        if ui.add(egui::Button::new(RichText::new(label).strong()).fill(ui.visuals().selection.bg_fill)).clicked() {
                            nav = Nav::Push(View::Reader(ReaderState::new(m.clone(), idx, page, self.store.settings.reader_mode)));
                        }
                    }
                    if ui.button(if fav { "★ In library" } else { "☆ Add to library" }).clicked() {
                        self.store.toggle_favourite(&m);
                    }
                    if !chapters.is_empty() {
                        ui.menu_button("⬇ Download", |ui| {
                            if ui.button("All chapters").clicked() {
                                let n = self.downloads.enqueue(&m, chapters.iter().cloned());
                                self.toast(format!("Queued {n} chapters"));
                                ui.close();
                            }
                            if ui.button("Unread chapters").clicked() {
                                let n = self.downloads.enqueue(&m, chapters.iter().filter(|c| !self.store.is_read(&m, c.id)).cloned());
                                self.toast(format!("Queued {n} chapters"));
                                ui.close();
                            }
                            if ui.button("Next 5 unread").clicked() {
                                let n = self.downloads.enqueue(&m, chapters.iter().filter(|c| !self.store.is_read(&m, c.id)).take(5).cloned());
                                self.toast(format!("Queued {n} chapters"));
                                ui.close();
                            }
                        });
                    }
                    if !chapters.is_empty() && ui.button("📤 Export").on_hover_text("Save downloaded chapters as CBZ or for the Xteink X4 (XTC)").clicked() {
                        self.open_export(&m, st.branch.clone());
                    }
                    if !m.public_url.is_empty() && ui.button("Web").on_hover_text("Open in browser").clicked() {
                        open_url(&m.public_url);
                    }
                });
                if let Some(d) = &m.description {
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().id_salt("desc").max_height(110.0).show(ui, |ui| {
                        ui.label(strip_html(d));
                    });
                }
            });
        });
        ui.separator();

        if let Some(e) = &st.error {
            if error_box(ui, e) {
                st.error = None;
            }
            return nav;
        }
        if st.pending.is_some() {
            spinner(ui);
            return nav;
        }

        // Chapter list
        let branches = {
            let mut b: Vec<Option<String>> = Vec::new();
            for c in chapters {
                if !b.contains(&c.branch) {
                    b.push(c.branch.clone());
                }
            }
            b
        };
        ui.horizontal(|ui| {
            ui.strong("Chapters");
            ui.add(egui::TextEdit::singleline(&mut st.filter).hint_text("Filter").desired_width(160.0));
            if ui.button(if st.reverse { "⬇ Newest first" } else { "⬆ Oldest first" }).clicked() {
                st.reverse = !st.reverse;
            }
            if branches.len() > 1 {
                let cur = st.branch.clone().unwrap_or_else(|| default_branch(&branches));
                let mut sel = cur.clone();
                egui::ComboBox::from_id_salt("branch").selected_text(cur.clone().unwrap_or_else(|| "Default".into())).show_ui(ui, |ui| {
                    for b in &branches {
                        ui.selectable_value(&mut sel, b.clone(), b.clone().unwrap_or_else(|| "Default".into()));
                    }
                });
                st.branch = Some(sel);
            }
        });
        let branch = if branches.len() > 1 { Some(st.branch.clone().unwrap_or_else(|| default_branch(&branches))) } else { None };
        let f = st.filter.to_lowercase();
        let mut idx: Vec<usize> = (0..chapters.len())
            .filter(|&i| branch.as_ref().is_none_or(|b| &chapters[i].branch == b))
            .filter(|&i| f.is_empty() || chapters[i].label().to_lowercase().contains(&f))
            .collect();
        if st.reverse {
            idx.reverse();
        }
        let mut mark = None;
        egui::ScrollArea::vertical().id_salt("chapters").auto_shrink([false, false]).show_rows(ui, 28.0, idx.len(), |ui, range| {
            for &i in &idx[range] {
                let c = &chapters[i];
                let read = self.store.is_read(&m, c.id);
                let current = last.as_ref().is_some_and(|h| h.chapter_id == c.id);
                ui.horizontal(|ui| {
                    ui.set_height(24.0);
                    let mut text = RichText::new(c.label());
                    if read {
                        text = text.color(Color32::from_gray(110));
                    }
                    if current {
                        text = text.strong().color(Color32::from_rgb(230, 140, 120));
                    }
                    let r = ui.add(egui::Button::new(text).frame(false));
                    if r.clicked() {
                        nav = Nav::Push(View::Reader(ReaderState::new(m.clone(), i, 0, self.store.settings.reader_mode)));
                    }
                    r.context_menu(|ui| {
                        if ui.button(if read { "Mark unread" } else { "Mark read" }).clicked() {
                            mark = Some((c.id, !read));
                            ui.close();
                        }
                        if ui.button("Mark this and older as read").clicked() {
                            for c2 in &chapters[..=i] {
                                self.store.mark_read(&m, c2.id);
                            }
                            ui.close();
                        }
                        if ui.button("Download").clicked() {
                            self.downloads.enqueue(&m, [c.clone()]);
                            ui.close();
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if downloads::is_downloaded(&m, c.id) {
                            ui.label(RichText::new("⬇").color(Color32::from_rgb(120, 180, 120))).on_hover_text("Downloaded");
                        }
                        if let Some(s) = &c.scanlator {
                            ui.label(RichText::new(s).small().weak());
                        }
                        if c.date > 0 {
                            ui.label(RichText::new(fmt_date(c.date)).small().weak());
                        }
                    });
                });
            }
        });
        if let Some((id, read)) = mark {
            if read {
                self.store.mark_read(&m, id);
            } else if let Some(s) = self.store.read.get_mut(&m.key()) {
                s.remove(&id);
                self.store.touch();
            }
        }
        nav
    }

    // ------------------------------------------------------------------ export

    /// Open the export dialog with the downloaded chapters of `m` (in the
    /// branch being viewed, if several translations are downloaded).
    fn open_export(&mut self, m: &Manga, branch: Option<Option<String>>) {
        let chapters: &[Chapter] = m.chapters.as_deref().unwrap_or(&[]);
        let mut downloaded: Vec<Chapter> = chapters.iter().filter(|c| downloads::is_downloaded(m, c.id)).cloned().collect();
        let mut branches: Vec<Option<String>> = vec![];
        for c in &downloaded {
            if !branches.contains(&c.branch) {
                branches.push(c.branch.clone());
            }
        }
        if branches.len() > 1 {
            let mut all: Vec<Option<String>> = vec![];
            for c in chapters {
                if !all.contains(&c.branch) {
                    all.push(c.branch.clone());
                }
            }
            let b = branch.unwrap_or_else(|| default_branch(&all));
            downloaded.retain(|c| c.branch == b);
        }
        let unread = downloaded.iter().filter(|c| !self.store.is_read(m, c.id)).cloned().collect();
        self.export_dialog = Some(ExportDialog { manga: m.slim(), downloaded, unread, only_unread: false });
    }

    fn export_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.export_dialog else { return };
        let s = &mut self.store.settings;
        let before = (s.export_dir.clone(), s.export_format, serde_json::to_string(&s.export_xtc).unwrap_or_default());
        let mut start = false;
        let mut close = false;
        let busy = self.export_job.is_some();
        let resp = egui::Modal::new(egui::Id::unique("export_dialog")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Export");
            ui.label(RichText::new(&d.manga.title).strong());
            ui.add_space(4.0);
            if d.downloaded.is_empty() {
                ui.label("No chapters of this manga are downloaded yet. Download some with ⬇ first.");
                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
                return;
            }
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.only_unread, false, format!("All downloaded ({})", d.downloaded.len()));
                ui.add_enabled_ui(!d.unread.is_empty(), |ui| {
                    ui.radio_value(&mut d.only_unread, true, format!("Unread only ({})", d.unread.len()));
                });
            });
            ui.add_space(6.0);
            ui.strong("Format");
            for f in [export::Format::Xtc, export::Format::Xtch, export::Format::Cbz] {
                ui.radio_value(&mut s.export_format, f, f.label());
            }
            if s.export_format != export::Format::Cbz {
                ui.add_space(6.0);
                ui.strong("Pages");
                let o = &mut s.export_xtc;
                egui::ComboBox::from_id_salt("export_layout").width(330.0).selected_text(o.layout.label()).show_ui(ui, |ui| {
                    for l in export::Layout::ALL {
                        ui.selectable_value(&mut o.layout, l, l.label());
                    }
                });
                ui.checkbox(&mut o.rtl, "Manga order (right page of a spread first)");
                ui.checkbox(&mut o.trim, "Crop white margins");
                ui.checkbox(&mut o.dither, "Dither (smoother shading)");
                ui.label(
                    RichText::new("Landscape: hold the X4 sideways. Copy the file to the SD card or send it with CrossPoint's Wi-Fi file transfer.")
                        .small()
                        .weak(),
                );
            } else {
                ui.checkbox(&mut s.export_xtc.rtl, "Mark as right-to-left manga");
            }
            ui.add_space(6.0);
            ui.strong("Save to");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut s.export_dir).desired_width(340.0));
                if ui.button("Open").on_hover_text("Open this folder").clicked() {
                    let _ = std::fs::create_dir_all(&s.export_dir);
                    open_url(&s.export_dir);
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let n = if d.only_unread { d.unread.len() } else { d.downloaded.len() };
                let label = if busy { "Another export is running…".to_string() } else { format!("Export {n} chapter{}", if n == 1 { "" } else { "s" }) };
                if ui.add_enabled(!busy && n > 0, egui::Button::new(RichText::new(label).strong()).fill(ui.visuals().selection.bg_fill)).clicked() {
                    start = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        let after = (s.export_dir.clone(), s.export_format, serde_json::to_string(&s.export_xtc).unwrap_or_default());
        if before != after {
            self.store.touch();
        }
        if start {
            let d = self.export_dialog.take().unwrap();
            let s = &self.store.settings;
            let chapters = if d.only_unread { d.unread } else { d.downloaded };
            let dir = std::path::PathBuf::from(&s.export_dir);
            let job = export::Job { manga: d.manga, chapters, dir, format: s.export_format, opts: s.export_xtc };
            self.export_job = Some(export::spawn(job, ctx.clone()));
            self.toast("Export started");
        } else if close || resp.should_close() {
            self.export_dialog = None;
        }
    }

    // ------------------------------------------------------------------ downloads

    fn downloads_ui(&mut self, ui: &mut Ui) -> Nav {
        let mut nav = Nav::None;
        ui.heading("Downloads");
        {
            let prog = self.downloads.progress.lock().unwrap();
            if let Some((t, c, d, n)) = &prog.current {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("{t} – {c}"));
                    ui.add(egui::ProgressBar::new(*d as f32 / (*n).max(1) as f32).desired_width(240.0).text(format!("{d}/{n}")));
                });
            }
            if !prog.failed.is_empty() {
                egui::CollapsingHeader::new(format!("{} failed", prog.failed.len())).show(ui, |ui| {
                    for f in &prog.failed {
                        ui.label(RichText::new(f).small());
                    }
                });
            }
        }
        let queued = self.downloads.queued();
        if !queued.is_empty() {
            ui.horizontal(|ui| {
                ui.label(format!("{} queued", queued.len()));
                if ui.small_button("Cancel queue").clicked() {
                    self.downloads.clear_queue();
                }
            });
        }
        ui.separator();
        let items = downloads::downloaded_manga();
        if items.is_empty() {
            ui.label("No downloads yet. Use ⬇ on a manga page.");
            return nav;
        }
        let out = manga_grid(ui, &mut self.images, "downloads", &items, &|_| None);
        if let Some(i) = out.clicked {
            nav = Nav::Push(View::Details(DetailsState::new(items[i].clone(), false)));
        }
        if let Some(i) = out.secondary {
            downloads::delete_manga(&items[i]);
            self.toast(format!("Deleted downloads of {}", items[i].title));
        }
        ui.label(RichText::new("Right-click a cover to delete its downloads").small().weak());
        nav
    }

    // ------------------------------------------------------------------ settings

    fn settings_ui(&mut self, ui: &mut Ui) -> Nav {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.settings_inner(ui));
        Nav::None
    }

    fn settings_inner(&mut self, ui: &mut Ui) {
        ui.heading("Settings");
        ui.separator();
        let s = &mut self.store.settings;
        let before = serde_json::to_string(s).unwrap_or_default();
        egui::Grid::new("settings").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
            ui.label("Default reader mode");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.reader_mode, ReaderMode::Webtoon, "Webtoon (scroll)");
                ui.selectable_value(&mut s.reader_mode, ReaderMode::Paged, "Paged L→R");
                ui.selectable_value(&mut s.reader_mode, ReaderMode::PagedRtl, "Paged R→L");
            });
            ui.end_row();
            ui.label("Webtoon max width");
            ui.add(egui::Slider::new(&mut s.reader_width, 400.0..=2400.0).suffix(" px"));
            ui.end_row();
            ui.label("Gap between pages");
            ui.add(egui::Slider::new(&mut s.reader_gap, 0.0..=40.0).suffix(" px"));
            ui.end_row();
            ui.label("Preload pages ahead");
            ui.add(egui::Slider::new(&mut s.preload_pages, 1..=12));
            ui.end_row();
            ui.label("UI scale");
            ui.add(egui::Slider::new(&mut s.ui_scale, 0.75..=2.0));
            ui.end_row();
            ui.label("Image memory budget");
            ui.add(egui::Slider::new(&mut s.texture_budget_mb, 128..=2048).suffix(" MB"));
            ui.end_row();
            ui.label("Show 18+ content");
            ui.checkbox(&mut s.show_adult, "");
            ui.end_row();
            ui.label("Show broken sources");
            ui.checkbox(&mut s.show_broken, "");
            ui.end_row();
        });
        if serde_json::to_string(&self.store.settings).unwrap_or_default() != before {
            self.store.touch();
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(format!("Images in memory: {} MB", self.images.memory_mb()));
            if ui.button("Clear image cache").clicked() {
                self.images.clear_memory();
                let _ = std::fs::remove_dir_all(crate::images::cache_dir());
                self.toast("Image cache cleared");
            }
        });
        ui.separator();
        self.sync_settings_ui(ui);
        ui.separator();
        ui.label(RichText::new(format!("Data: {}", store::data_dir().display())).small().weak());
        ui.label(RichText::new(format!("{} sources loaded", self.sources.len())).small().weak());
        ui.add_space(10.0);
        ui.label(RichText::new("Reader keys: ←/→ or A/D page · Space/PgDn scroll · N/P next/prev chapter · M switch mode · F fullscreen · Esc back").small());
    }
}

fn default_branch(branches: &[Option<String>]) -> Option<String> {
    branches
        .iter()
        .find(|b| b.as_deref().is_some_and(|s| s.to_lowercase().contains("english") || s.eq_ignore_ascii_case("en")))
        .cloned()
        .unwrap_or_else(|| branches.first().cloned().flatten())
}

pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let s = s.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n").replace("</p>", "\n");
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").trim().to_string()
}

fn fmt_date(ms: i64) -> String {
    // civil-from-days (Howard Hinnant)
    let days = ms / 86_400_000;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}
