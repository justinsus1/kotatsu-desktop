//! Persistent user data: library, history, read markers, settings. One small
//! JSON file, written atomically and only when something changed.

use crate::model::Manga;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReaderMode {
    #[default]
    Webtoon,
    Paged,
    PagedRtl,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub reader_mode: ReaderMode,
    pub reader_width: f32,
    pub show_adult: bool,
    pub show_broken: bool,
    pub languages: Vec<String>,
    pub texture_budget_mb: usize,
    pub preload_pages: usize,
    pub ui_scale: f32,
    pub reader_gap: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            reader_mode: ReaderMode::Webtoon,
            reader_width: 900.0,
            show_adult: false,
            show_broken: false,
            languages: vec!["en".into()],
            texture_budget_mb: 512,
            preload_pages: 4,
            ui_scale: 1.0,
            reader_gap: 0.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Favourite {
    pub manga: Manga,
    pub added: i64,
    /// Chapter count when last checked, to detect new chapters.
    #[serde(default)]
    pub known_chapters: usize,
    #[serde(default)]
    pub new_chapters: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub manga: Manga,
    pub chapter_id: i64,
    pub chapter_label: String,
    pub page: usize,
    pub at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub settings: Settings,
    pub favourites: Vec<Favourite>,
    pub history: Vec<HistoryEntry>,
    /// manga key -> read chapter ids
    pub read: HashMap<String, HashSet<i64>>,
    pub pinned_sources: Vec<String>,
    #[serde(skip)]
    dirty: bool,
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("kotatsu-desktop")
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Store {
    fn path() -> PathBuf {
        data_dir().join("store.json")
    }

    pub fn load() -> Store {
        std::fs::read(Self::path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_else(|| Store {
            pinned_sources: vec!["MANGADEX".into()],
            ..Default::default()
        })
    }

    pub fn touch(&mut self) {
        self.dirty = true;
    }

    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let path = Self::path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let tmp = path.with_extension("json.tmp");
        if let Ok(data) = serde_json::to_vec(self) {
            if std::fs::write(&tmp, data).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    pub fn is_favourite(&self, m: &Manga) -> bool {
        self.favourites.iter().any(|f| f.manga.source == m.source && f.manga.id == m.id)
    }

    pub fn toggle_favourite(&mut self, m: &Manga) {
        if let Some(i) = self.favourites.iter().position(|f| f.manga.source == m.source && f.manga.id == m.id) {
            self.favourites.remove(i);
        } else {
            let n = m.chapters.as_ref().map_or(0, Vec::len);
            self.favourites.insert(0, Favourite { manga: m.slim(), added: now(), known_chapters: n, new_chapters: 0 });
        }
        self.touch();
    }

    pub fn last_read(&self, m: &Manga) -> Option<&HistoryEntry> {
        self.history.iter().find(|h| h.manga.source == m.source && h.manga.id == m.id)
    }

    pub fn record(&mut self, m: &Manga, chapter_id: i64, label: &str, page: usize) {
        let entry = HistoryEntry { manga: m.slim(), chapter_id, chapter_label: label.to_string(), page, at: now() };
        self.history.retain(|h| !(h.manga.source == m.source && h.manga.id == m.id));
        self.history.insert(0, entry);
        self.history.truncate(500);
        self.touch();
    }

    pub fn mark_read(&mut self, m: &Manga, chapter_id: i64) {
        if self.read.entry(m.key()).or_default().insert(chapter_id) {
            self.touch();
        }
    }

    pub fn is_read(&self, m: &Manga, chapter_id: i64) -> bool {
        self.read.get(&m.key()).is_some_and(|s| s.contains(&chapter_id))
    }

    pub fn toggle_pin(&mut self, source: &str) {
        if let Some(i) = self.pinned_sources.iter().position(|s| s == source) {
            self.pinned_sources.remove(i);
        } else {
            self.pinned_sources.push(source.to_string());
        }
        self.touch();
    }
}
