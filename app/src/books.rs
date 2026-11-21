//! Books: an EPUB shelf (a folder, `~/Books` by default), a Project Gutenberg
//! catalogue to download public-domain books from, and the EPUB reader.

use crate::app::{App, Nav, View};
use crate::bridge::Pending;
use crate::epub::{self, Block, Book, BookMeta, Kind};
use crate::images::{Img, ImgSrc};
use crate::model::Manga;
use crate::store::{now, BookProgress, BookTheme};
use crate::widgets::*;
use egui::text::{LayoutJob, TextFormat};
use egui::{pos2, vec2, Color32, FontFamily, FontId, Galley, Key, Rect, RichText, Sense, Ui};
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

// ------------------------------------------------------------------ fonts

/// Extra font families for book text. Called once at startup.
pub fn add_fonts(fonts: &mut egui::FontDefinitions) {
    let dir = "/usr/share/fonts/noto";
    let sets = [
        ("serif", "NotoSerif-Regular.ttf"),
        ("serif-b", "NotoSerif-Bold.ttf"),
        ("serif-i", "NotoSerif-Italic.ttf"),
        ("serif-bi", "NotoSerif-BoldItalic.ttf"),
        ("sans", "NotoSans-Regular.ttf"),
        ("sans-b", "NotoSans-Bold.ttf"),
        ("sans-i", "NotoSans-Italic.ttf"),
        ("sans-bi", "NotoSans-BoldItalic.ttf"),
    ];
    // Book fonts fall back to whatever the UI uses (CJK etc.)
    let fallback: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (name, file) in sets {
        let Ok(data) = std::fs::read(format!("{dir}/{file}")) else { continue };
        fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(data)));
        let mut fam = vec![name.to_string()];
        fam.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(name.into()), fam);
    }
}

fn family(ctx: &egui::Context, serif: bool, bold: bool, italic: bool) -> (FontFamily, bool) {
    let base = if serif { "serif" } else { "sans" };
    let name = match (bold, italic) {
        (false, false) => base.to_string(),
        (true, false) => format!("{base}-b"),
        (false, true) => format!("{base}-i"),
        (true, true) => format!("{base}-bi"),
    };
    let has = |n: &str| ctx.fonts(|f| f.families().iter().any(|x| *x == FontFamily::Name(n.into())));
    if has(&name) {
        (FontFamily::Name(name.into()), false)
    } else if has(base) {
        (FontFamily::Name(base.into()), italic)
    } else {
        (FontFamily::Proportional, italic)
    }
}

fn theme_colors(t: BookTheme) -> (Color32, Color32, Color32) {
    match t {
        BookTheme::Dark => (Color32::from_rgb(22, 22, 24), Color32::from_rgb(222, 220, 214), Color32::from_rgb(120, 170, 230)),
        BookTheme::Sepia => (Color32::from_rgb(244, 236, 216), Color32::from_rgb(70, 54, 40), Color32::from_rgb(140, 70, 30)),
        BookTheme::Light => (Color32::from_rgb(252, 252, 250), Color32::from_rgb(25, 25, 25), Color32::from_rgb(30, 90, 190)),
    }
}

// ------------------------------------------------------------------ shelf

pub struct Shelf {
    pub books: Vec<BookMeta>,
    scan: Option<crossbeam_channel::Receiver<Vec<BookMeta>>>,
    filter: String,
    pub tab: ShelfTab,
    store: GutenbergState,
    /// None = Project Gutenberg, Some(i) = OPDS catalogue i
    source: Option<usize>,
    opds: OpdsState,
    adding: Option<crate::store::Catalog>,
    auth_sent: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ShelfTab {
    Mine,
    Get,
}

impl Shelf {
    pub fn new(cached: Vec<BookMeta>) -> Self {
        Shelf {
            books: cached,
            scan: None,
            filter: String::new(),
            tab: ShelfTab::Mine,
            store: GutenbergState::default(),
            source: None,
            opds: OpdsState::default(),
            adding: None,
            auth_sent: false,
        }
    }
}

pub fn books_dir(app: &App) -> PathBuf {
    PathBuf::from(&app.store.settings.books_dir)
}

fn book_as_manga(b: &BookMeta) -> Manga {
    Manga {
        id: u64::from_str_radix(&crate::images::hash_str(&b.path.to_string_lossy()), 16).unwrap_or(0) as i64,
        title: b.title.clone(),
        url: b.path.to_string_lossy().into_owned(),
        cover: b.cover.clone(),
        authors: vec![b.author.clone()],
        source: BOOK.into(),
        ..Default::default()
    }
}

impl App {
    pub fn rescan_books(&mut self) {
        if self.shelf.scan.is_some() {
            return;
        }
        let dir = books_dir(self);
        let known: HashMap<PathBuf, BookMeta> = self.shelf.books.iter().map(|b| (b.path.clone(), b.clone())).collect();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let bridge = self.bridge.clone();
        std::thread::spawn(move || {
            let _ = std::fs::create_dir_all(&dir);
            let mut files = epub::scan(&dir);
            // A local Calibre library is picked up automatically
            if let Some(cal) = dirs::home_dir().map(|h| h.join("Calibre Library")) {
                if cal.is_dir() && !dir.starts_with(&cal) && !cal.starts_with(&dir) {
                    files.extend(epub::scan(&cal));
                }
            }
            let mut out: Vec<BookMeta> = files
                .into_iter()
                .filter_map(|p| known.get(&p).cloned().or_else(|| epub::read_meta(&p).ok()))
                .collect();
            out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
            let _ = tx.send(out);
            bridge.repaint();
        });
        self.shelf.scan = Some(rx);
    }

    pub fn poll_books(&mut self, ctx: &egui::Context) {
        if let Some(books) = self.shelf.scan.as_ref().and_then(|r| r.try_recv().ok()) {
            self.shelf.scan = None;
            self.store.book_cache = books.clone();
            self.store.touch();
            self.shelf.books = books;
        }
        // Drag & drop .epub files onto the window to add them
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            let dir = books_dir(self);
            let _ = std::fs::create_dir_all(&dir);
            let mut n = 0;
            for p in dropped.iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub"))) {
                if let Some(name) = p.file_name() {
                    if std::fs::copy(p, dir.join(name)).is_ok() {
                        n += 1;
                    }
                }
            }
            if n > 0 {
                self.toast(format!("Added {n} book(s) to {}", dir.display()));
                self.rescan_books();
            }
        }
        self.poll_gutenberg();
        self.poll_opds();
        if !self.shelf.auth_sent {
            self.shelf.auth_sent = true;
            for c in self.store.catalogs.clone() {
                self.send_auth(&c);
            }
        }
    }

    pub fn books_ui(&mut self, ui: &mut Ui) -> Nav {
        let mut nav = Nav::None;
        ui.horizontal(|ui| {
            ui.heading("Books");
            ui.selectable_value(&mut self.shelf.tab, ShelfTab::Mine, "My books");
            ui.selectable_value(&mut self.shelf.tab, ShelfTab::Get, "Get books");
        });
        ui.separator();
        match self.shelf.tab {
            ShelfTab::Mine => {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.shelf.filter).hint_text("Filter").desired_width(200.0));
                    if ui.button("Rescan").clicked() {
                        self.rescan_books();
                    }
                    if ui.button("Open folder").clicked() {
                        open_url(&self.store.settings.books_dir);
                    }
                    if self.shelf.scan.is_some() {
                        ui.spinner();
                    }
                    ui.label(RichText::new(format!("{} · drop .epub files on the window to add", self.store.settings.books_dir)).small().weak());
                });
                let q = self.shelf.filter.to_lowercase();
                let mut list: Vec<&BookMeta> = self
                    .shelf
                    .books
                    .iter()
                    .filter(|b| q.is_empty() || b.title.to_lowercase().contains(&q) || b.author.to_lowercase().contains(&q))
                    .collect();
                // Recently read first
                let at = |b: &BookMeta| self.store.books.get(&*b.path.to_string_lossy()).map_or(0, |p| p.at);
                list.sort_by_key(|b| std::cmp::Reverse(at(b)));
                if list.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.label("No books yet. Get free books from \"Get books\", or put .epub files in your books folder.");
                    });
                    return nav;
                }
                let metas: Vec<BookMeta> = list.into_iter().cloned().collect();
                let items: Vec<Manga> = metas.iter().map(book_as_manga).collect();
                let progress: Vec<Option<f32>> = metas.iter().map(|b| self.store.books.get(&*b.path.to_string_lossy()).map(|p| p.percent)).collect();
                let badge = |i: usize| progress[i].filter(|p| *p > 0.0).map(|p| format!("{:.0}%", p * 100.0));
                let out = manga_grid(ui, &mut self.images, "books", &items, &badge);
                if let Some(i) = out.clicked {
                    nav = Nav::Push(View::Book(BookReader::new(metas[i].path.clone())));
                }
            }
            ShelfTab::Get => {
                ui.horizontal(|ui| {
                    ui.label("Source:");
                    let names: Vec<String> = self.store.catalogs.iter().map(|c| c.name.clone()).collect();
                    let cur = match self.shelf.source {
                        Some(i) => names.get(i).cloned().unwrap_or_default(),
                        None => "Project Gutenberg".into(),
                    };
                    let mut sel = self.shelf.source;
                    egui::ComboBox::from_id_salt("bsrc").selected_text(cur).show_ui(ui, |ui| {
                        ui.selectable_value(&mut sel, None, "Project Gutenberg");
                        for (i, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut sel, Some(i), n);
                        }
                    });
                    if sel != self.shelf.source {
                        self.shelf.source = sel;
                        self.shelf.opds = OpdsState::default();
                    }
                    if ui.button("+ Add catalogue").on_hover_text("Calibre content server, Kavita, Komga, COPS… (OPDS)").clicked() {
                        self.shelf.adding = Some(crate::store::Catalog { name: "Calibre".into(), url: "http://localhost:8080/opds".into(), ..Default::default() });
                    }
                    if let Some(i) = self.shelf.source {
                        if ui.button("Remove").clicked() {
                            self.store.catalogs.remove(i);
                            self.store.touch();
                            self.shelf.source = None;
                            self.shelf.opds = OpdsState::default();
                        }
                    }
                });
                self.add_catalog_window(ui);
                nav = match self.shelf.source {
                    None => self.gutenberg_ui(ui),
                    Some(i) => self.opds_ui(ui, i),
                };
            }
        }
        nav
    }
}

// ------------------------------------------------------------------ Project Gutenberg

#[derive(Clone, Debug, Deserialize)]
struct GAuthor {
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GBook {
    id: i64,
    title: String,
    #[serde(default)]
    authors: Vec<GAuthor>,
    #[serde(default)]
    formats: HashMap<String, String>,
}

#[derive(Deserialize)]
struct GPage {
    next: Option<String>,
    results: Vec<GBook>,
}

#[derive(Default)]
struct GutenbergState {
    query: String,
    active: String,
    items: Vec<GBook>,
    next: Option<String>,
    pending: Option<Pending>,
    started: bool,
    end: bool,
    error: Option<String>,
    downloading: HashMap<i64, Pending>,
}

const MIRRORS: [&str; 3] = ["https://www.gutenberg.org/", "https://gutenberg.pglaf.org/", "https://aleph.pglaf.org/"];

/// The same Project Gutenberg file on its mirrors.
pub fn mirrors(url: &str) -> Vec<String> {
    match url.strip_prefix(MIRRORS[0]) {
        Some(rest) => MIRRORS.iter().map(|m| format!("{m}{rest}")).collect(),
        None => vec![url.to_string()],
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect::<String>().chars().take(90).collect()
}

fn pg_tag(id: i64) -> String {
    format!("[pg{id}]")
}

impl App {
    fn gutenberg_request(&mut self) {
        let st = &mut self.shelf.store;
        let url = match &st.next {
            Some(n) if !st.items.is_empty() => n.clone(),
            _ => {
                let lang = &self.store.settings.book_lang;
                let mut u = format!("https://gutendex.com/books/?mime_type=application%2Fepub");
                if !lang.is_empty() {
                    u.push_str(&format!("&languages={lang}"));
                }
                if !st.active.is_empty() {
                    let q: String = st.active.split_whitespace().collect::<Vec<_>>().join("%20");
                    u.push_str(&format!("&search={q}"));
                }
                u
            }
        };
        st.pending = Some(self.bridge.call("http", json!({"method": "GET", "url": url})));
    }

    fn poll_gutenberg(&mut self) {
        let st = &mut self.shelf.store;
        if let Some(r) = st.pending.as_ref().and_then(|p| p.ready().cloned()) {
            st.pending = None;
            let parsed = r.and_then(|v| {
                let code = v["code"].as_u64().unwrap_or(0);
                if code != 200 {
                    return Err(format!("catalogue returned HTTP {code}"));
                }
                serde_json::from_str::<GPage>(v["body"].as_str().unwrap_or("")).map_err(|e| e.to_string())
            });
            match parsed {
                Ok(p) => {
                    st.end = p.next.is_none() || p.results.is_empty();
                    st.next = p.next;
                    st.items.extend(p.results);
                }
                Err(e) => st.error = Some(e),
            }
        }
        let done: Vec<i64> = st.downloading.iter().filter(|(_, p)| p.ready().is_some()).map(|(id, _)| *id).collect();
        let mut msgs = vec![];
        let mut rescan = false;
        for id in done {
            let p = st.downloading.remove(&id).unwrap();
            let title = st.items.iter().find(|b| b.id == id).map(|b| b.title.clone()).unwrap_or_default();
            match p.ready().unwrap() {
                Ok(_) => {
                    msgs.push(format!("Downloaded “{title}”"));
                    rescan = true;
                }
                Err(e) => msgs.push(format!("Download failed: {e}")),
            }
        }
        for m in msgs {
            self.toast(m);
        }
        if rescan {
            self.rescan_books();
        }
    }

    fn gutenberg_ui(&mut self, ui: &mut Ui) -> Nav {
        let mut nav = Nav::None;
        let mut reset = false;
        ui.horizontal(|ui| {
            let st = &mut self.shelf.store;
            let r = ui.add(egui::TextEdit::singleline(&mut st.query).hint_text("Search title or author").desired_width(280.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && st.query != st.active {
                st.active = st.query.clone();
                reset = true;
            }
            let mut lang = self.store.settings.book_lang.clone();
            egui::ComboBox::from_id_salt("glang").selected_text(if lang.is_empty() { "Any language".into() } else { lang.clone() }).show_ui(ui, |ui| {
                for l in ["", "en", "fr", "de", "es", "it", "pt", "nl", "fi", "zh", "ja", "la"] {
                    ui.selectable_value(&mut lang, l.to_string(), if l.is_empty() { "Any language" } else { l });
                }
            });
            if lang != self.store.settings.book_lang {
                self.store.settings.book_lang = lang;
                self.store.touch();
                reset = true;
            }
            ui.label(RichText::new("Free public-domain books from Project Gutenberg").small().weak());
        });
        if reset {
            let st = &mut self.shelf.store;
            st.items.clear();
            st.next = None;
            st.end = false;
            st.error = None;
            st.pending = None;
            st.started = false;
        }
        ui.separator();
        if !self.shelf.store.started {
            self.shelf.store.started = true;
            self.gutenberg_request();
        }
        if let Some(e) = self.shelf.store.error.clone() {
            if error_box(ui, &e) {
                self.shelf.store.error = None;
                self.gutenberg_request();
            }
            return nav;
        }
        if self.shelf.store.items.is_empty() {
            if self.shelf.store.pending.is_some() {
                spinner(ui);
            } else {
                ui.label("Nothing found.");
            }
            return nav;
        }
        let owned: HashSet<i64> = self
            .shelf
            .books
            .iter()
            .filter_map(|b| {
                let n = b.path.file_name()?.to_string_lossy().into_owned();
                let s = n.rfind("[pg")?;
                n[s + 3..].split(']').next()?.parse().ok()
            })
            .collect();
        let st = &self.shelf.store;
        let items: Vec<Manga> = st
            .items
            .iter()
            .map(|b| Manga {
                id: b.id,
                title: b.title.clone(),
                url: format!("pg:{}", b.id),
                cover: b.formats.get("image/jpeg").cloned(),
                authors: b.authors.iter().map(|a| a.name.clone()).collect(),
                source: WEB_BOOK.into(),
                ..Default::default()
            })
            .collect();
        let ids: Vec<i64> = st.items.iter().map(|b| b.id).collect();
        let downloading: HashSet<i64> = st.downloading.keys().copied().collect();
        let badge = |i: usize| {
            if owned.contains(&ids[i]) {
                Some("✓".to_string())
            } else if downloading.contains(&ids[i]) {
                Some("…".to_string())
            } else {
                None
            }
        };
        let out = manga_grid(ui, &mut self.images, "gutenberg", &items, &badge);
        ui.label(RichText::new("Click to download (✓ = in your library, click again to read)").small().weak());
        if out.near_end && self.shelf.store.pending.is_none() && !self.shelf.store.end {
            self.gutenberg_request();
        }
        if let Some(i) = out.clicked {
            let b = self.shelf.store.items[i].clone();
            if let Some(path) = self.shelf.books.iter().find(|m| m.path.to_string_lossy().contains(&pg_tag(b.id))).map(|m| m.path.clone()) {
                nav = Nav::Push(View::Book(BookReader::new(path)));
            } else if !self.shelf.store.downloading.contains_key(&b.id) {
                let author = b.authors.first().map(|a| a.name.as_str()).unwrap_or("Unknown");
                let dest = books_dir(self).join("Gutenberg").join(format!("{} - {} {}.epub", sanitize(&b.title), sanitize(author), pg_tag(b.id)));
                let file = format!("https://www.gutenberg.org/cache/epub/{0}/pg{0}-images-3.epub", b.id);
                let mut urls = mirrors(&file);
                if let Some(u) = b.formats.get("application/epub+zip") {
                    urls.push(u.clone());
                }
                let p = self.bridge.call("download", json!({"urls": urls, "dest": dest}));
                self.shelf.store.downloading.insert(b.id, p);
                self.toast(format!("Downloading “{}”…", b.title));
            }
        }
        nav
    }
}

// ------------------------------------------------------------------ reader

type Loaded = Result<Vec<Block>, String>;

pub struct BookReader {
    path: PathBuf,
    book: Option<Result<Book, String>>,
    open_job: Option<crossbeam_channel::Receiver<Result<Book, String>>>,
    spine: usize,
    chapters: HashMap<usize, Arc<Vec<Block>>>,
    loading: HashMap<usize, crossbeam_channel::Receiver<Loaded>>,
    error: Option<String>,
    /// laid-out text, keyed by the layout parameters
    galleys: Vec<Option<Arc<Galley>>>,
    layout_key: Option<(usize, u32, u32, u32, bool, BookTheme)>,
    img_aspect: HashMap<usize, f32>,
    restore_block: Option<usize>,
    tops: Vec<f32>,
    block: usize,
    show_toc: bool,
    show_bar: bool,
    started: bool,
}

impl BookReader {
    pub fn new(path: PathBuf) -> Self {
        BookReader {
            path,
            book: None,
            open_job: None,
            spine: 0,
            chapters: HashMap::new(),
            loading: HashMap::new(),
            error: None,
            galleys: vec![],
            layout_key: None,
            img_aspect: HashMap::new(),
            restore_block: None,
            tops: vec![],
            block: 0,
            show_toc: false,
            show_bar: true,
            started: false,
        }
    }

    fn key(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    fn spine_len(&self) -> usize {
        match &self.book {
            Some(Ok(b)) => b.spine.len(),
            _ => 0,
        }
    }

    fn request_chapter(&mut self, i: usize, repaint: Arc<dyn Fn() + Send + Sync>) {
        if self.chapters.contains_key(&i) || self.loading.contains_key(&i) {
            return;
        }
        let Some(Ok(book)) = &self.book else { return };
        let Some(entry) = book.spine.get(i).cloned() else { return };
        let path = self.path.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(epub::load_chapter(&path, &entry));
            repaint();
        });
        self.loading.insert(i, rx);
    }

    fn goto(&mut self, spine: usize, block: usize) {
        self.spine = spine;
        self.restore_block = Some(block);
        self.layout_key = None;
        self.img_aspect.clear();
        // keep memory small: only neighbours stay parsed
        self.chapters.retain(|k, _| k.abs_diff(spine) <= 1);
    }
}

impl App {
    pub fn book_reader_ui(&mut self, ui: &mut Ui, st: &mut BookReader) -> Nav {
        let mut nav = Nav::None;
        let ctx = ui.ctx().clone();
        let bridge = self.bridge.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || bridge.repaint());

        // ---- open the book
        if st.book.is_none() && st.open_job.is_none() {
            let path = st.path.clone();
            let (tx, rx) = crossbeam_channel::bounded(1);
            let r = repaint.clone();
            std::thread::spawn(move || {
                let _ = tx.send(epub::open_book(&path));
                r();
            });
            st.open_job = Some(rx);
        }
        if let Some(b) = st.open_job.as_ref().and_then(|r| r.try_recv().ok()) {
            st.open_job = None;
            st.book = Some(b);
        }
        if let Some(Ok(_)) = &st.book {
            if !st.started {
                st.started = true;
                let p = self.store.books.get(&st.key()).cloned().unwrap_or_default();
                st.goto(p.spine.min(st.spine_len().saturating_sub(1)), p.block);
            }
        }
        // ---- chapter loading (+ prefetch next)
        let done: Vec<usize> = st.loading.iter().filter_map(|(k, r)| r.try_recv().ok().map(|v| (*k, v))).map(|(k, v)| {
            match v {
                Ok(blocks) => {
                    st.chapters.insert(k, Arc::new(blocks));
                }
                Err(e) => {
                    if k == st.spine {
                        st.error = Some(e);
                    }
                }
            }
            k
        }).collect();
        for k in done {
            st.loading.remove(&k);
        }
        if st.started {
            st.request_chapter(st.spine, repaint.clone());
            if st.chapters.contains_key(&st.spine) {
                st.request_chapter(st.spine + 1, repaint.clone());
            }
        }

        let s = &self.store.settings;
        let (bg, fg, accent) = theme_colors(s.book_theme);
        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);

        // ---- keys
        let typing = ctx.memory(|m| m.focused().is_some());
        let mut font_delta = 0.0;
        let (mut next_ch, mut prev_ch) = (false, false);
        if !typing {
            ctx.input(|i| {
                if i.key_pressed(Key::Escape) || i.pointer.button_pressed(egui::PointerButton::Extra1) {
                    nav = Nav::Back;
                }
                next_ch |= i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::N);
                prev_ch |= i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::P);
                if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
                    font_delta = 1.0;
                }
                if i.key_pressed(Key::Minus) {
                    font_delta = -1.0;
                }
                if i.key_pressed(Key::T) {
                    st.show_toc = !st.show_toc;
                }
                if i.key_pressed(Key::Tab) || i.key_pressed(Key::H) {
                    st.show_bar = !st.show_bar;
                }
                if i.key_pressed(Key::F) || i.key_pressed(Key::F11) {
                    let fs = i.viewport().fullscreen.unwrap_or(false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                }
            });
        }

        let title = match &st.book {
            Some(Ok(b)) => b.meta.title.clone(),
            _ => st.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        let n_spine = st.spine_len();

        // ---- top bar
        if st.show_bar {
            egui::Panel::top("book-bar").frame(egui::Frame::NONE.fill(bg).inner_margin(6)).show(ui, |ui| {
                ui.visuals_mut().override_text_color = Some(fg);
                ui.horizontal(|ui| {
                    if ui.button("←").clicked() {
                        nav = Nav::Back;
                    }
                    if ui.button("Contents").on_hover_text("T").clicked() {
                        st.show_toc = !st.show_toc;
                    }
                    ui.label(RichText::new(&title).strong());
                    if n_spine > 0 {
                        if ui.add_enabled(st.spine > 0, egui::Button::new("◀")).on_hover_text("Previous chapter (←)").clicked() {
                            prev_ch = true;
                        }
                        if ui.add_enabled(st.spine + 1 < n_spine, egui::Button::new("▶")).on_hover_text("Next chapter (→)").clicked() {
                            next_ch = true;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let s = &mut self.store.settings;
                        let mut changed = false;
                        let theme_label = match s.book_theme {
                            BookTheme::Dark => "Dark",
                            BookTheme::Sepia => "Sepia",
                            BookTheme::Light => "Light",
                        };
                        if ui.button(theme_label).on_hover_text("Theme").clicked() {
                            s.book_theme = match s.book_theme {
                                BookTheme::Dark => BookTheme::Sepia,
                                BookTheme::Sepia => BookTheme::Light,
                                BookTheme::Light => BookTheme::Dark,
                            };
                            changed = true;
                        }
                        if ui.button(if s.book_serif { "Serif" } else { "Sans" }).on_hover_text("Font").clicked() {
                            s.book_serif = !s.book_serif;
                            changed = true;
                        }
                        if ui.button("A+").clicked() {
                            font_delta = 1.0;
                        }
                        if ui.button("A−").clicked() {
                            font_delta = -1.0;
                        }
                        ui.menu_button("Layout", |ui| {
                            changed |= ui.add(egui::Slider::new(&mut s.book_width, 400.0..=1400.0).text("Width")).changed();
                            changed |= ui.add(egui::Slider::new(&mut s.book_line, 1.1..=2.2).text("Line spacing")).changed();
                        });
                        if changed {
                            self.store.touch();
                        }
                    });
                });
            });
        }
        if font_delta != 0.0 {
            let s = &mut self.store.settings;
            s.book_font = (s.book_font + font_delta).clamp(11.0, 40.0);
            self.store.touch();
        }
        if next_ch && st.spine + 1 < n_spine {
            st.goto(st.spine + 1, 0);
        }
        if prev_ch && st.spine > 0 {
            st.goto(st.spine - 1, 0);
        }

        // ---- table of contents
        if st.show_toc {
            if let Some(Ok(book)) = &st.book {
                let toc = book.toc.clone();
                let mut jump = None;
                egui::Panel::left("toc").frame(egui::Frame::NONE.fill(bg).inner_margin(8)).default_size(280.0).show(ui, |ui| {
                    ui.visuals_mut().override_text_color = Some(fg);
                    ui.strong("Contents");
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if toc.is_empty() {
                            for i in 0..n_spine {
                                if ui.selectable_label(i == st.spine, format!("Section {}", i + 1)).clicked() {
                                    jump = Some(i);
                                }
                            }
                        }
                        for t in &toc {
                            ui.horizontal(|ui| {
                                ui.add_space(t.depth as f32 * 14.0);
                                if ui.selectable_label(t.spine == st.spine, &t.title).clicked() {
                                    jump = Some(t.spine);
                                }
                            });
                        }
                    });
                });
                if let Some(j) = jump {
                    st.goto(j, 0);
                }
            }
        }

        if !matches!(nav, Nav::None) {
            self.save_book(st);
            return nav;
        }
        if let Some(Err(e)) = &st.book {
            ui.colored_label(Color32::from_rgb(230, 110, 110), format!("Can't open this book: {e}"));
            return nav;
        }
        if let Some(e) = &st.error {
            ui.colored_label(Color32::from_rgb(230, 110, 110), e);
            return nav;
        }
        let Some(blocks) = st.chapters.get(&st.spine).cloned() else {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.spinner();
            });
            return nav;
        };

        // ---- layout (cached per chapter + parameters)
        let s = self.store.settings.clone();
        let avail = ui.available_size();
        let width = (avail.x - 48.0).min(s.book_width).max(200.0);
        let key = (st.spine, width as u32, (s.book_font * 10.0) as u32, (s.book_line * 100.0) as u32, s.book_serif, s.book_theme);
        if st.layout_key != Some(key) {
            st.layout_key = Some(key);
            st.galleys = blocks.iter().map(|b| layout_block(&ctx, ui, b, width, &s, fg, accent)).collect();
        }
        let para_gap = s.book_font * 0.7;
        let mut tops = Vec::with_capacity(blocks.len() + 1);
        let mut y = 24.0;
        for (i, b) in blocks.iter().enumerate() {
            tops.push(y);
            let h = match (b, &st.galleys[i]) {
                (Block::Text { kind, .. }, Some(g)) => g.size().y + if matches!(kind, Kind::Heading(_)) { para_gap * 1.6 } else { para_gap },
                (Block::Image { .. }, _) => width * st.img_aspect.get(&i).copied().unwrap_or(0.6) + para_gap,
                (Block::Rule, _) => para_gap * 2.5,
                _ => 0.0,
            };
            y += h;
        }
        tops.push(y);
        let total = y;

        let mut area = egui::ScrollArea::vertical().id_salt(("book", st.spine)).auto_shrink([false, false]);
        if let Some(b) = st.restore_block.take() {
            area = area.vertical_scroll_offset(tops[b.min(blocks.len())] - 8.0);
        } else if st.tops.len() == tops.len() && st.tops != tops {
            // keep the current paragraph in place when images above it load
            let shift = tops[st.block.min(blocks.len())] - st.tops[st.block.min(blocks.len())];
            if shift.abs() > 0.5 {
                let cur = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::unique(("book-y", st.spine)))).unwrap_or(0.0);
                area = area.vertical_scroll_offset(cur + shift);
            }
        }
        st.tops = tops.clone();
        let (k_down, k_up, k_pgdn, k_pgup) = if typing {
            Default::default()
        } else {
            ui.input(|i| {
                (
                    i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::J),
                    i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::K),
                    i.key_pressed(Key::PageDown) || (i.key_pressed(Key::Space) && !i.modifiers.shift),
                    i.key_pressed(Key::PageUp) || (i.key_pressed(Key::Space) && i.modifiers.shift),
                )
            })
        };
        let mut want_next = false;
        let ppp = ctx.pixels_per_point();
        let path = st.path.clone();
        let out = area.show_viewport(ui, |ui, vp| {
            let end_h = 140.0;
            ui.set_height(total + end_h);
            ui.set_width(avail.x);
            let origin = ui.min_rect().min;
            let x0 = origin.x + ((avail.x - width) / 2.0).floor();
            let step = vp.height() * 0.92;
            let delta = if k_down { 80.0 } else if k_up { -80.0 } else if k_pgdn { step } else if k_pgup { -step } else { 0.0 };
            if delta != 0.0 {
                ui.scroll_with_delta(vec2(0.0, -delta));
            }
            let first = tops.partition_point(|&t| t <= vp.min.y).saturating_sub(1);
            let last = tops.partition_point(|&t| t < vp.max.y).min(blocks.len());
            st.block = tops.partition_point(|&t| t <= vp.min.y + 4.0).saturating_sub(1).min(blocks.len().saturating_sub(1));
            for i in first..last {
                let y = origin.y + tops[i];
                match &blocks[i] {
                    Block::Text { kind, .. } => {
                        if let Some(g) = &st.galleys[i] {
                            let indent = match kind {
                                Kind::Quote => 28.0,
                                Kind::ListItem => 18.0,
                                _ => 0.0,
                            };
                            let x = if matches!(kind, Kind::Heading(_)) { x0 + (width - g.size().x) / 2.0 } else { x0 + indent };
                            if *kind == Kind::Quote {
                                ui.painter().vline(x0 + 10.0, y..=y + g.size().y, egui::Stroke::new(2.0, accent.gamma_multiply(0.5)));
                            }
                            ui.painter().galley(pos2(x, y), g.clone(), fg);
                        }
                    }
                    Block::Image { entry } => {
                        let key = format!("bi:{}:{entry}", path.display());
                        let src = || ImgSrc::Zip { file: path.clone(), entry: entry.clone() };
                        match self.images.get(&key, src, (width * ppp) as u32) {
                            Img::Ready { tiles, size } => {
                                let w = (size[0] as f32 / ppp).min(width);
                                let a = size[1] as f32 / size[0].max(1) as f32;
                                let h = w * a;
                                let full_a = h / width;
                                if (st.img_aspect.get(&i).copied().unwrap_or(-1.0) - full_a).abs() > 0.001 {
                                    st.img_aspect.insert(i, full_a);
                                    ui.ctx().request_repaint();
                                }
                                let r = Rect::from_min_size(pos2(x0 + (width - w) / 2.0, y), vec2(w, h));
                                crate::images::paint_tiles(ui, r, tiles, size);
                            }
                            Img::Loading => {}
                            Img::Failed(_) => {
                                st.img_aspect.insert(i, 0.0);
                            }
                        }
                    }
                    Block::Rule => {
                        let yy = y + para_gap;
                        ui.painter().hline(x0 + width * 0.35..=x0 + width * 0.65, yy, egui::Stroke::new(1.0, fg.gamma_multiply(0.4)));
                    }
                }
            }
            let end = Rect::from_min_size(pos2(origin.x, origin.y + total), vec2(avail.x, end_h));
            if ui.is_rect_visible(end) && st.spine + 1 < n_spine {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(end.shrink(30.0)).layout(egui::Layout::top_down(egui::Align::Center)));
                child.visuals_mut().override_text_color = Some(fg);
                if child.button(RichText::new("Next chapter  ▶").size(16.0)).clicked() {
                    want_next = true;
                }
            }
            if vp.max.y >= total + end_h - 2.0 {
                st.block = blocks.len().saturating_sub(1);
            }
        });
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique(("book-y", st.spine)), out.state.clamped_offset().y));
        if want_next {
            st.goto(st.spine + 1, 0);
        }

        // progress footer
        let pct = progress(st, blocks.len());
        ui.painter().text(
            ui.max_rect().right_bottom() - vec2(14.0, 8.0),
            egui::Align2::RIGHT_BOTTOM,
            format!("{:.0}%", pct * 100.0),
            FontId::proportional(12.0),
            fg.gamma_multiply(0.5),
        );
        self.save_book(st);
        nav
    }

    fn save_book(&mut self, st: &BookReader) {
        if !st.started || st.spine_len() == 0 {
            return;
        }
        let n = st.chapters.get(&st.spine).map_or(1, |c| c.len());
        let p = BookProgress { spine: st.spine, block: st.block, percent: progress(st, n), at: now() };
        let key = st.key();
        let changed = self.store.books.get(&key).is_none_or(|o| o.spine != p.spine || o.block != p.block);
        if changed {
            self.store.books.insert(key, p);
            self.store.touch();
        }
    }
}

fn progress(st: &BookReader, blocks: usize) -> f32 {
    let n = st.spine_len().max(1) as f32;
    ((st.spine as f32 + (st.block + 1) as f32 / blocks.max(1) as f32) / n).clamp(0.0, 1.0)
}

fn layout_block(ctx: &egui::Context, ui: &Ui, b: &Block, width: f32, s: &crate::store::Settings, fg: Color32, accent: Color32) -> Option<Arc<Galley>> {
    let Block::Text { kind, spans } = b else { return None };
    let base = s.book_font;
    let (size, force_bold) = match kind {
        Kind::Heading(1) => (base * 1.75, true),
        Kind::Heading(2) => (base * 1.5, true),
        Kind::Heading(3) => (base * 1.3, true),
        Kind::Heading(_) => (base * 1.12, true),
        Kind::Pre => (base * 0.85, false),
        _ => (base, false),
    };
    let indent = match kind {
        Kind::Quote => 28.0,
        Kind::ListItem => 18.0,
        _ => 0.0,
    };
    let mut job = LayoutJob::default();
    job.wrap.max_width = width - indent;
    if matches!(kind, Kind::Heading(_)) {
        job.halign = egui::Align::LEFT;
    }
    if *kind == Kind::ListItem {
        let (fam, _) = family(ctx, s.book_serif, false, false);
        job.append("• ", 0.0, TextFormat { font_id: FontId::new(size, fam), color: fg, line_height: Some(size * s.book_line), ..Default::default() });
    }
    for sp in spans {
        let bold = sp.bold || force_bold;
        let italic = sp.italic || *kind == Kind::Quote;
        let (fam, fake_italic) = if sp.mono { (FontFamily::Monospace, italic) } else { family(ctx, s.book_serif, bold, italic) };
        let color = if sp.link { accent } else { fg };
        job.append(
            &sp.text,
            0.0,
            TextFormat { font_id: FontId::new(size, fam), color, italics: fake_italic, line_height: Some(size * s.book_line), ..Default::default() },
        );
    }
    Some(ui.painter().layout_job(job))
}

// ------------------------------------------------------------------ OPDS catalogues (Calibre etc.)

#[derive(Default)]
pub struct OpdsState {
    /// (title, url) of the feeds opened so far; last = current
    stack: Vec<(String, String)>,
    title: String,
    entries: Vec<crate::opds::Entry>,
    next: Option<String>,
    search: Option<String>,
    pending: Option<(String, Pending)>,
    osd: Option<Pending>,
    query: String,
    error: Option<String>,
    downloading: HashMap<String, Pending>,
}

fn host_keys(url: &str) -> Vec<String> {
    let https = url.starts_with("https://");
    let rest = url.split("://").nth(1).unwrap_or(url);
    let hostport = rest.split(['/', '?']).next().unwrap_or("");
    let hostport = hostport.rsplit('@').next().unwrap_or(hostport);
    match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => vec![format!("{h}:{p}")],
        _ => vec![format!("{hostport}:{}", if https { 443 } else { 80 })],
    }
}

#[cfg(test)]
pub fn test_b64(s: &str) -> String {
    b64(s)
}

fn b64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

impl App {
    fn send_auth(&self, c: &crate::store::Catalog) {
        let header = if c.user.is_empty() { String::new() } else { format!("Basic {}", b64(&format!("{}:{}", c.user, c.pass))) };
        for h in host_keys(&c.url) {
            self.bridge.call("setAuth", json!({"host": h, "header": header}));
        }
    }

    fn add_catalog_window(&mut self, ui: &mut Ui) {
        let Some(mut c) = self.shelf.adding.take() else { return };
        let mut open = true;
        let mut save = false;
        egui::Window::new("Add OPDS catalogue").collapsible(false).resizable(false).open(&mut open).show(ui.ctx(), |ui| {
            ui.label(RichText::new("Calibre: run the content server (Preferences → Sharing over the net), then use http://<computer>:8080/opds").small().weak());
            egui::Grid::new("addcat").num_columns(2).show(ui, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut c.name);
                ui.end_row();
                ui.label("URL");
                ui.add(egui::TextEdit::singleline(&mut c.url).desired_width(320.0));
                ui.end_row();
                ui.label("Username");
                ui.text_edit_singleline(&mut c.user);
                ui.end_row();
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(&mut c.pass).password(true));
                ui.end_row();
            });
            ui.label(RichText::new("Optional. Basic auth only (for Calibre with a password, set the server's authentication mode to \"basic\").").small().weak());
            if ui.add_enabled(!c.url.trim().is_empty(), egui::Button::new("Add")).clicked() {
                save = true;
            }
        });
        if save {
            c.url = c.url.trim().to_string();
            if !c.url.contains("://") {
                c.url = format!("http://{}", c.url);
            }
            self.send_auth(&c);
            self.store.catalogs.push(c);
            self.store.touch();
            self.shelf.source = Some(self.store.catalogs.len() - 1);
            self.shelf.opds = OpdsState::default();
        } else if open {
            self.shelf.adding = Some(c);
        }
    }

    fn opds_open(&mut self, title: String, url: String, push: bool) {
        let st = &mut self.shelf.opds;
        if push {
            st.stack.push((title, url.clone()));
        }
        st.entries.clear();
        st.next = None;
        st.error = None;
        st.pending = Some((url.clone(), self.bridge.call("http", json!({"method": "GET", "url": url, "headers": {"Accept": "application/atom+xml"}}))));
    }

    fn poll_opds(&mut self) {
        let st = &mut self.shelf.opds;
        if let Some((url, p)) = &st.pending {
            if let Some(r) = p.ready().cloned() {
                let url = url.clone();
                st.pending = None;
                let res = r.and_then(|v| {
                    match v["code"].as_u64().unwrap_or(0) {
                        200..=299 => {}
                        401 => return Err("The catalogue needs a username and password (or they are wrong).".into()),
                        c => return Err(format!("HTTP {c}")),
                    }
                    crate::opds::parse_feed(v["body"].as_str().unwrap_or(""), &url)
                });
                match res {
                    Ok(f) => {
                        if st.entries.is_empty() {
                            st.title = f.title;
                        }
                        st.entries.extend(f.entries);
                        st.next = f.next;
                        if let Some(s) = f.search {
                            st.search = Some(if s.contains("://") || s.contains("{searchTerms}") { crate::opds::join(&url, &s) } else { s });
                        }
                    }
                    Err(e) => st.error = Some(e),
                }
            }
        }
        if let Some(r) = st.osd.as_ref().and_then(|p| p.ready().cloned()) {
            st.osd = None;
            let base = st.search.clone().unwrap_or_default();
            st.search = r.ok().and_then(|v| crate::opds::parse_opensearch(v["body"].as_str().unwrap_or(""), &base));
            if st.search.is_some() && !st.query.trim().is_empty() {
                let q = st.query.clone();
                self.opds_search(&q);
            }
            return;
        }
        let done: Vec<String> = st.downloading.iter().filter(|(_, p)| p.ready().is_some()).map(|(k, _)| k.clone()).collect();
        let mut msgs = vec![];
        for k in done {
            if let Some(p) = st.downloading.remove(&k) {
                msgs.push(match p.ready().unwrap() {
                    Ok(_) => "Book downloaded".to_string(),
                    Err(e) => format!("Download failed: {e}"),
                });
            }
        }
        if !msgs.is_empty() {
            for m in msgs {
                self.toast(m);
            }
            self.rescan_books();
        }
    }

    fn opds_search(&mut self, q: &str) {
        let Some(t) = self.shelf.opds.search.clone() else { return };
        if t.contains("{searchTerms}") {
            let url = crate::opds::search_url(&t, q.trim());
            self.opds_open(format!("Search: {}", q.trim()), url, true);
        } else if self.shelf.opds.osd.is_none() {
            // OpenSearch description: fetch it first, then search
            self.shelf.opds.osd = Some(self.bridge.call("http", json!({"method": "GET", "url": t})));
        }
    }

    fn opds_ui(&mut self, ui: &mut Ui, idx: usize) -> Nav {
        let mut nav = Nav::None;
        let Some(cat) = self.store.catalogs.get(idx).cloned() else { return nav };
        if self.shelf.opds.stack.is_empty() {
            self.opds_open(cat.name.clone(), cat.url.clone(), true);
        }
        let mut back = None;
        let mut search = None;
        ui.horizontal(|ui| {
            let st = &mut self.shelf.opds;
            if st.stack.len() > 1 && ui.button("←").clicked() {
                back = Some(());
            }
            ui.strong(st.stack.last().map(|s| s.0.clone()).unwrap_or_default());
            if st.search.is_some() {
                let r = ui.add(egui::TextEdit::singleline(&mut st.query).hint_text("Search catalogue").desired_width(240.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !st.query.trim().is_empty() {
                    search = Some(st.query.clone());
                }
            }
            if st.pending.is_some() || st.osd.is_some() {
                ui.spinner();
            }
        });
        if back.is_some() {
            self.shelf.opds.stack.pop();
            if let Some((t, u)) = self.shelf.opds.stack.last().cloned() {
                self.opds_open(t, u, false);
            }
        }
        if let Some(q) = search {
            self.opds_search(&q);
        }
        ui.separator();
        if let Some(e) = self.shelf.opds.error.clone() {
            if error_box(ui, &e) {
                if let Some((t, u)) = self.shelf.opds.stack.last().cloned() {
                    self.opds_open(t, u, false);
                }
            }
            return nav;
        }
        let entries = self.shelf.opds.entries.clone();
        let navs: Vec<&crate::opds::Entry> = entries.iter().filter(|e| e.epub.is_none() && e.nav.is_some()).collect();
        let books: Vec<&crate::opds::Entry> = entries.iter().filter(|e| e.epub.is_some()).collect();
        if entries.is_empty() && self.shelf.opds.pending.is_none() {
            ui.label("This catalogue page is empty (or has no EPUB books).");
        }
        let mut open = None;
        if !navs.is_empty() {
            let max_h = if books.is_empty() { f32::INFINITY } else { 160.0 };
            egui::ScrollArea::vertical().id_salt("opds-nav").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                for e in &navs {
                    if ui.add(egui::Button::new(format!("📁 {}", e.title)).frame(false)).clicked() {
                        open = Some((e.title.clone(), e.nav.clone().unwrap()));
                    }
                }
            });
            if !books.is_empty() {
                ui.separator();
            }
        }
        if let Some((t, u)) = open {
            self.opds_open(t, u, true);
            return nav;
        }
        if books.is_empty() {
            return nav;
        }
        let tag = |e: &crate::opds::Entry| format!("[cat-{}]", &crate::images::hash_str(&format!("{}{}", cat.url, if e.id.is_empty() { e.epub.as_deref().unwrap_or("") } else { &e.id }))[..8]);
        let owned: HashSet<String> = self.shelf.books.iter().filter_map(|b| b.path.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        let tags: Vec<String> = books.iter().map(|e| tag(e)).collect();
        let is_owned = |t: &str| owned.iter().any(|n| n.contains(t));
        let items: Vec<Manga> = books
            .iter()
            .map(|e| Manga { title: e.title.clone(), url: e.epub.clone().unwrap_or_default(), cover: e.cover.clone(), authors: vec![e.author.clone()], source: WEB_BOOK.into(), ..Default::default() })
            .collect();
        let downloading: HashSet<String> = self.shelf.opds.downloading.keys().cloned().collect();
        let badge = |i: usize| {
            if is_owned(&tags[i]) {
                Some("✓".to_string())
            } else if downloading.contains(&tags[i]) {
                Some("…".to_string())
            } else {
                None
            }
        };
        let out = manga_grid(ui, &mut self.images, ("opds", idx), &items, &badge);
        if out.near_end && self.shelf.opds.pending.is_none() {
            if let Some(n) = self.shelf.opds.next.take() {
                let st = &mut self.shelf.opds;
                st.pending = Some((n.clone(), self.bridge.call("http", json!({"method": "GET", "url": n, "headers": {"Accept": "application/atom+xml"}}))));
            }
        }
        if let Some(i) = out.clicked {
            let e = books[i];
            let t = &tags[i];
            if let Some(b) = self.shelf.books.iter().find(|b| b.path.to_string_lossy().contains(t.as_str())) {
                nav = Nav::Push(View::Book(BookReader::new(b.path.clone())));
            } else if !self.shelf.opds.downloading.contains_key(t) {
                let dest = books_dir(self).join(sanitize(&cat.name)).join(format!("{} - {} {}.epub", sanitize(&e.title), sanitize(&e.author), t));
                let p = self.bridge.call("download", json!({"urls": [e.epub.clone().unwrap()], "dest": dest}));
                self.shelf.opds.downloading.insert(t.clone(), p);
                self.toast(format!("Downloading “{}”…", e.title));
            }
        }
        nav
    }
}
