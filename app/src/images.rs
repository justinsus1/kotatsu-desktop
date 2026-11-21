//! Image pipeline: fetch (via bridge, into a disk cache) -> decode -> downscale ->
//! split into GPU-sized tiles on worker threads; textures are created on the UI
//! thread and evicted LRU against a VRAM budget.

use crate::bridge::Bridge;
use crate::model::Page;
use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};
use fast_image_resize as fr;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const TILE_H: u32 = 4096;

#[derive(Clone)]
pub enum ImgSrc {
    Url { source: String, url: String },
    Page { source: String, page: Page },
    File(PathBuf),
}

struct Job {
    key: String,
    src: ImgSrc,
    max_w: u32,
}

struct Done {
    key: String,
    result: Result<(Vec<ColorImage>, [u32; 2]), String>,
}

pub struct Tile {
    pub tex: TextureHandle,
    pub h: u32,
}

enum Entry {
    Loading,
    Ready { tiles: Vec<Tile>, size: [u32; 2], bytes: usize },
    Failed(String),
}

struct Slot {
    entry: Entry,
    used: Instant,
    max_w: u32,
}

pub enum Img<'a> {
    Loading,
    Ready { tiles: &'a [Tile], size: [u32; 2] },
    Failed(&'a str),
}

impl Img<'_> {
    pub fn aspect(&self) -> Option<f32> {
        match self {
            Img::Ready { size, .. } => Some(size[1] as f32 / size[0].max(1) as f32),
            _ => None,
        }
    }
}

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    cv: Condvar,
    wanted: Mutex<HashSet<String>>,
}

pub struct Images {
    slots: HashMap<String, Slot>,
    queue: Arc<Queue>,
    rx: crossbeam_channel::Receiver<Done>,
    budget: usize,
    total: usize,
}

pub fn hash_str(s: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("kotatsu-desktop/img")
}

fn cache_path(key: &str) -> PathBuf {
    let h = hash_str(key);
    cache_dir().join(&h[..2]).join(h)
}

impl Images {
    pub fn new(bridge: Arc<Bridge>, ctx: egui::Context, budget_mb: usize) -> Images {
        let queue = Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), cv: Condvar::new(), wanted: Default::default() });
        let (tx, rx) = crossbeam_channel::unbounded();
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(4, 10);
        for i in 0..workers {
            let q = queue.clone();
            let tx = tx.clone();
            let bridge = bridge.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name(format!("img-{i}"))
                .spawn(move || {
                    let mut resizer = fr::Resizer::new();
                    loop {
                        let job = {
                            let mut g = q.jobs.lock().unwrap();
                            loop {
                                // LIFO: whatever was asked for most recently is what is on screen now
                                if let Some(j) = g.pop_back() {
                                    break j;
                                }
                                g = q.cv.wait(g).unwrap();
                            }
                        };
                        if !q.wanted.lock().unwrap().contains(&job.key) {
                            continue;
                        }
                        let result = load(&bridge, &mut resizer, &job);
                        q.wanted.lock().unwrap().remove(&job.key);
                        let _ = tx.send(Done { key: job.key, result });
                        ctx.request_repaint();
                    }
                })
                .unwrap();
        }
        std::thread::spawn(|| prune_disk_cache(1500 << 20));
        Images { slots: HashMap::new(), queue, rx, budget: budget_mb << 20, total: 0 }
    }

    /// Request an image; returns its current state. Call every frame it is visible.
    pub fn get(&mut self, key: &str, src: impl FnOnce() -> ImgSrc, max_w: u32) -> Img<'_> {
        let now = Instant::now();
        let max_w = max_w.max(64);
        let need_load = match self.slots.get_mut(key) {
            Some(s) => {
                s.used = now;
                // Re-decode at higher resolution when the view grows a lot
                matches!(s.entry, Entry::Ready { size, .. } if max_w > s.max_w + s.max_w / 3 && size[0] >= s.max_w)
            }
            None => true,
        };
        if need_load {
            let had_ready = matches!(self.slots.get(key), Some(Slot { entry: Entry::Ready { .. }, .. }));
            if !had_ready {
                self.slots.insert(key.to_string(), Slot { entry: Entry::Loading, used: now, max_w });
            } else if let Some(s) = self.slots.get_mut(key) {
                s.max_w = max_w;
            }
            self.queue.wanted.lock().unwrap().insert(key.to_string());
            self.queue.jobs.lock().unwrap().push_back(Job { key: key.to_string(), src: src(), max_w });
            self.queue.cv.notify_one();
        }
        match &self.slots[key].entry {
            Entry::Loading => Img::Loading,
            Entry::Ready { tiles, size, .. } => Img::Ready { tiles, size: *size },
            Entry::Failed(e) => Img::Failed(e),
        }
    }

    /// Warm an image without drawing it (prefetch).
    pub fn prefetch(&mut self, key: &str, src: impl FnOnce() -> ImgSrc, max_w: u32) {
        let _ = self.get(key, src, max_w);
    }

    pub fn retry(&mut self, key: &str) {
        if matches!(self.slots.get(key), Some(Slot { entry: Entry::Failed(_), .. })) {
            self.slots.remove(key);
        }
    }

    /// Drain finished jobs into textures and enforce the memory budget.
    pub fn end_frame(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.rx.try_recv() {
            let Some(slot) = self.slots.get_mut(&done.key) else { continue };
            match done.result {
                Ok((imgs, size)) => {
                    let mut bytes = 0;
                    let tiles = imgs
                        .into_iter()
                        .enumerate()
                        .map(|(i, img)| {
                            bytes += img.pixels.len() * 4;
                            let h = img.size[1] as u32;
                            Tile { tex: ctx.load_texture(format!("{}#{i}", done.key), img, TextureOptions::LINEAR), h }
                        })
                        .collect();
                    if let Entry::Ready { bytes: old, .. } = slot.entry {
                        self.total -= old;
                    }
                    self.total += bytes;
                    slot.entry = Entry::Ready { tiles, size, bytes };
                }
                Err(e) => {
                    if !matches!(slot.entry, Entry::Ready { .. }) {
                        slot.entry = Entry::Failed(e);
                    }
                }
            }
        }

        let now = Instant::now();
        // Forget queued work nobody has looked at for a while
        let stale: Vec<String> = self
            .slots
            .iter()
            .filter(|(_, s)| matches!(s.entry, Entry::Loading | Entry::Failed(_)) && now - s.used > Duration::from_secs(4))
            .map(|(k, _)| k.clone())
            .collect();
        if !stale.is_empty() {
            let mut w = self.queue.wanted.lock().unwrap();
            for k in stale {
                w.remove(&k);
                self.slots.remove(&k);
            }
        }

        if self.total > self.budget {
            let mut ready: Vec<(Instant, String)> = self
                .slots
                .iter()
                .filter(|(_, s)| matches!(s.entry, Entry::Ready { .. }) && now - s.used > Duration::from_millis(500))
                .map(|(k, s)| (s.used, k.clone()))
                .collect();
            ready.sort_unstable();
            for (_, k) in ready {
                if self.total <= self.budget * 3 / 4 {
                    break;
                }
                if let Some(Slot { entry: Entry::Ready { bytes, .. }, .. }) = self.slots.remove(&k) {
                    self.total -= bytes;
                }
            }
        }
    }

    pub fn clear_memory(&mut self) {
        self.slots.clear();
        self.total = 0;
    }

    pub fn memory_mb(&self) -> usize {
        self.total >> 20
    }
}

fn load(bridge: &Bridge, resizer: &mut fr::Resizer, job: &Job) -> Result<(Vec<ColorImage>, [u32; 2]), String> {
    let path = match &job.src {
        ImgSrc::File(p) => p.clone(),
        ImgSrc::Url { source, url } => {
            let p = cache_path(&job.key);
            if !p.exists() {
                bridge.call_blocking("fetch", json!({"source": source, "url": url, "dest": p}))?;
            }
            p
        }
        ImgSrc::Page { source, page } => {
            let p = cache_path(&job.key);
            if !p.exists() {
                bridge.call_blocking("fetch", json!({"source": source, "page": page, "dest": p}))?;
            }
            p
        }
    };
    let data = std::fs::read(&path).map_err(|e| e.to_string())?;
    let img = match image::load_from_memory(&data) {
        Ok(i) => i,
        Err(e) => {
            // Corrupt cache entries must not stick around
            if !matches!(job.src, ImgSrc::File(_)) {
                let _ = std::fs::remove_file(&path);
            }
            return Err(format!("decode: {e}"));
        }
    };
    drop(data);
    let mut rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    if w > job.max_w {
        let nw = job.max_w;
        let nh = ((h as u64 * nw as u64) / w as u64).max(1) as u32;
        let src = fr::images::Image::from_vec_u8(w, h, rgba.into_raw(), fr::PixelType::U8x4).map_err(|e| e.to_string())?;
        let mut dst = fr::images::Image::new(nw, nh, fr::PixelType::U8x4);
        let opts = fr::ResizeOptions::new().resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::CatmullRom));
        resizer.resize(&src, &mut dst, &opts).map_err(|e| e.to_string())?;
        rgba = image::RgbaImage::from_raw(nw, nh, dst.into_vec()).ok_or("resize")?;
    }
    let (w, h) = rgba.dimensions();
    let raw = rgba.into_raw();
    let row = w as usize * 4;
    let mut tiles = Vec::with_capacity((h / TILE_H + 1) as usize);
    let mut y = 0;
    while y < h {
        let th = TILE_H.min(h - y);
        let slice = &raw[y as usize * row..(y + th) as usize * row];
        tiles.push(ColorImage::from_rgba_unmultiplied([w as usize, th as usize], slice));
        y += th;
    }
    Ok((tiles, [w, h]))
}

fn prune_disk_cache(limit: u64) {
    let mut files = Vec::new();
    let Ok(dirs) = std::fs::read_dir(cache_dir()) else { return };
    for d in dirs.flatten() {
        if let Ok(rd) = std::fs::read_dir(d.path()) {
            for f in rd.flatten() {
                if let Ok(m) = f.metadata() {
                    let t = m.accessed().or_else(|_| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                    files.push((t, m.len(), f.path()));
                }
            }
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= limit {
        return;
    }
    files.sort_unstable_by_key(|f| f.0);
    for (_, len, p) in files {
        if total <= limit * 4 / 5 {
            break;
        }
        if std::fs::remove_file(p).is_ok() {
            total -= len;
        }
    }
}

/// Paint an image's tiles stacked vertically into `rect`.
pub fn paint_tiles(ui: &egui::Ui, rect: egui::Rect, tiles: &[Tile], size: [u32; 2]) {
    let scale = rect.height() / size[1].max(1) as f32;
    let mut y = rect.top();
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    for t in tiles {
        let h = t.h as f32 * scale;
        let r = egui::Rect::from_min_size(egui::pos2(rect.left(), y), Vec2::new(rect.width(), h));
        if ui.is_rect_visible(r) {
            ui.painter().image(t.tex.id(), r, uv, egui::Color32::WHITE);
        }
        y += h;
    }
}
