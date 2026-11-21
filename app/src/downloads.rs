//! Offline downloads: chapters are saved as numbered image files plus an
//! `index.json`, processed by one background thread with parallel page fetches.

use crate::bridge::Bridge;
use crate::model::{Chapter, Manga, Page};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone)]
pub struct Task {
    pub manga: Manga,
    pub chapter: Chapter,
}

#[derive(Default)]
pub struct Progress {
    pub current: Option<(String, String, usize, usize)>, // manga title, chapter, done, total
    pub failed: Vec<String>,
    pub completed: usize,
}

#[derive(Serialize, Deserialize)]
struct Index {
    pages: usize,
    files: Vec<String>,
}

pub struct Downloads {
    queue: Arc<(Mutex<VecDeque<Task>>, Condvar)>,
    pub progress: Arc<Mutex<Progress>>,
}

pub fn root() -> PathBuf {
    crate::store::data_dir().join("downloads")
}

pub fn manga_dir(m: &Manga) -> PathBuf {
    root().join(&m.source).join(m.id.to_string())
}

pub fn chapter_dir(m: &Manga, ch_id: i64) -> PathBuf {
    manga_dir(m).join(ch_id.to_string())
}

/// Local page files for a downloaded chapter, if complete.
pub fn local_pages(m: &Manga, ch_id: i64) -> Option<Vec<PathBuf>> {
    let dir = chapter_dir(m, ch_id);
    let idx: Index = serde_json::from_slice(&std::fs::read(dir.join("index.json")).ok()?).ok()?;
    Some(idx.files.iter().map(|f| dir.join(f)).collect())
}

pub fn is_downloaded(m: &Manga, ch_id: i64) -> bool {
    chapter_dir(m, ch_id).join("index.json").exists()
}

/// All manga with at least one downloaded chapter.
pub fn downloaded_manga() -> Vec<Manga> {
    let mut out = Vec::new();
    let Ok(sources) = std::fs::read_dir(root()) else { return out };
    for s in sources.flatten() {
        let Ok(mangas) = std::fs::read_dir(s.path()) else { continue };
        for m in mangas.flatten() {
            if let Ok(d) = std::fs::read(m.path().join("manga.json")) {
                if let Ok(manga) = serde_json::from_slice::<Manga>(&d) {
                    out.push(manga);
                }
            }
        }
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

pub fn delete_manga(m: &Manga) {
    let _ = std::fs::remove_dir_all(manga_dir(m));
}

impl Downloads {
    pub fn new(bridge: Arc<Bridge>, ctx: egui::Context) -> Downloads {
        let queue: Arc<(Mutex<VecDeque<Task>>, Condvar)> = Default::default();
        let progress: Arc<Mutex<Progress>> = Default::default();
        let (q, p) = (queue.clone(), progress.clone());
        std::thread::Builder::new()
            .name("downloads".into())
            .spawn(move || loop {
                let task = {
                    let mut g = q.0.lock().unwrap();
                    loop {
                        if let Some(t) = g.pop_front() {
                            break t;
                        }
                        g = q.1.wait(g).unwrap();
                    }
                };
                if let Err(e) = run(&bridge, &task, &p, &ctx) {
                    p.lock().unwrap().failed.push(format!("{} – {}: {e}", task.manga.title, task.chapter.label()));
                } else {
                    p.lock().unwrap().completed += 1;
                }
                p.lock().unwrap().current = None;
                ctx.request_repaint();
            })
            .unwrap();
        Downloads { queue, progress }
    }

    pub fn enqueue(&self, manga: &Manga, chapters: impl IntoIterator<Item = Chapter>) -> usize {
        let mut g = self.queue.0.lock().unwrap();
        let mut n = 0;
        for ch in chapters {
            if is_downloaded(manga, ch.id) || g.iter().any(|t| t.chapter.id == ch.id && t.manga.source == manga.source) {
                continue;
            }
            g.push_back(Task { manga: manga.slim(), chapter: ch });
            n += 1;
        }
        self.queue.1.notify_one();
        n
    }

    pub fn queued(&self) -> Vec<(String, String)> {
        self.queue.0.lock().unwrap().iter().map(|t| (t.manga.title.clone(), t.chapter.label())).collect()
    }

    pub fn clear_queue(&self) {
        self.queue.0.lock().unwrap().clear();
    }
}

fn run(bridge: &Bridge, task: &Task, prog: &Mutex<Progress>, ctx: &egui::Context) -> Result<(), String> {
    let m = &task.manga;
    let pages: Vec<Page> = serde_json::from_value(
        bridge.call_blocking("pages", json!({"source": m.source, "chapter": task.chapter}))?,
    )
    .map_err(|e| e.to_string())?;
    let total = pages.len();
    let dir = chapter_dir(m, task.chapter.id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(manga_dir(m).join("manga.json"), serde_json::to_vec(m).unwrap()).map_err(|e| e.to_string())?;
    prog.lock().unwrap().current = Some((m.title.clone(), task.chapter.label(), 0, total));
    ctx.request_repaint();

    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let err: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= total || err.lock().unwrap().is_some() {
                    break;
                }
                let dest = dir.join(format!("{i:04}"));
                if !dest.exists() {
                    let mut last = Ok(serde_json::Value::Null);
                    for _ in 0..3 {
                        last = bridge.call_blocking("fetch", json!({"source": m.source, "page": pages[i], "dest": dest}));
                        if last.is_ok() {
                            break;
                        }
                    }
                    if let Err(e) = last {
                        *err.lock().unwrap() = Some(e);
                        break;
                    }
                }
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                if let Some(c) = prog.lock().unwrap().current.as_mut() {
                    c.2 = d;
                }
                ctx.request_repaint();
            });
        }
    });
    if let Some(e) = err.into_inner().unwrap() {
        return Err(e);
    }
    let idx = Index { pages: total, files: (0..total).map(|i| format!("{i:04}")).collect() };
    std::fs::write(dir.join("index.json"), serde_json::to_vec(&idx).unwrap()).map_err(|e| e.to_string())?;
    Ok(())
}
