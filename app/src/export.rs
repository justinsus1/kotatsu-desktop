//! Export downloaded chapters for other readers:
//! - CBZ: JPEG pages in one folder per chapter, plus ComicInfo.xml.
//! - XTC / XTCH: the Xteink X4's native format, pre-rendered 480×800 pages
//!   (1-bit or 4-level grey) with a chapter table. Readable on stock firmware
//!   and CrossPoint without any converter.
//!
//! The layout follows the files xtcjs writes (known to open on CrossPoint and
//! stock firmware): header(56) | title(128) author(112) toc-header(16) |
//! chapters(96 each) | page index(16 each) | pages.

use crate::downloads;
use crate::model::{Chapter, Manga};
use fast_image_resize as fr;
use image::GrayImage;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const X4_W: u32 = 480;
pub const X4_H: u32 = 800;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Format {
    Cbz,
    /// 1-bit pages: fastest page turns.
    #[default]
    Xtc,
    /// 4-level grey pages: better art, slower turns.
    Xtch,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Cbz => "CBZ (for XTC.js, CBZ firmware, other readers)",
            Format::Xtc => "XTC for Xteink X4 – black & white, fastest",
            Format::Xtch => "XTCH for Xteink X4 – 4 greys, nicer art",
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Format::Cbz => "cbz",
            Format::Xtc => "xtc",
            Format::Xtch => "xtch",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Layout {
    /// Each page cut into overlapping horizontal strips, read with the device
    /// held sideways. Text stays readable on the 4.3" screen.
    #[default]
    LandscapeThirds,
    /// Top and bottom half, sideways.
    LandscapeHalves,
    /// The whole page, upright. Small text.
    Portrait,
}

impl Layout {
    pub const ALL: [Layout; 3] = [Layout::LandscapeThirds, Layout::LandscapeHalves, Layout::Portrait];
    pub fn label(self) -> &'static str {
        match self {
            Layout::LandscapeThirds => "Landscape, overlapping thirds (recommended)",
            Layout::LandscapeHalves => "Landscape, halves",
            Layout::Portrait => "Portrait, whole page",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct XtcOptions {
    pub layout: Layout,
    /// Manga order: right page of a two-page spread first.
    pub rtl: bool,
    pub dither: bool,
    /// Crop white margins before fitting to the screen.
    pub trim: bool,
}

impl Default for XtcOptions {
    fn default() -> Self {
        XtcOptions { layout: Layout::LandscapeThirds, rtl: true, dither: true, trim: true }
    }
}

pub struct Job {
    pub manga: Manga,
    /// In reading order (oldest first); must all be downloaded.
    pub chapters: Vec<Chapter>,
    pub dir: PathBuf,
    pub format: Format,
    pub opts: XtcOptions,
}

pub enum Msg {
    Progress(usize, usize),
    Done(Result<PathBuf, String>),
}

pub struct Running {
    pub rx: crossbeam_channel::Receiver<Msg>,
    pub cancel: Arc<AtomicBool>,
    pub progress: (usize, usize),
    pub title: String,
}

pub fn spawn(job: Job, ctx: egui::Context) -> Running {
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    let title = job.manga.title.clone();
    std::thread::Builder::new()
        .name("export".into())
        .spawn(move || {
            let tx2 = tx.clone();
            let ctx2 = ctx.clone();
            let progress = move |d: usize, n: usize| {
                let _ = tx2.send(Msg::Progress(d, n));
                ctx2.request_repaint();
            };
            let r = run(&job, &c, &progress);
            let _ = tx.send(Msg::Done(r));
            ctx.request_repaint();
        })
        .unwrap();
    Running { rx, cancel, progress: (0, 0), title }
}

/// Export synchronously. `progress(done, total)` counts source pages.
pub fn run(job: &Job, cancel: &AtomicBool, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<PathBuf, String> {
    if job.chapters.is_empty() {
        return Err("no downloaded chapters to export".into());
    }
    let mut chapters = Vec::new();
    for c in &job.chapters {
        let pages = downloads::local_pages(&job.manga, c.id).ok_or_else(|| format!("{} is not downloaded", c.label()))?;
        chapters.push((c, pages));
    }
    std::fs::create_dir_all(&job.dir).map_err(|e| format!("{}: {e}", job.dir.display()))?;
    let name = file_name(&job.manga, &job.chapters, job.format.ext());
    let dest = job.dir.join(name);
    let part = dest.with_extension("part");
    let r = match job.format {
        Format::Cbz => write_cbz(job, &chapters, &part, cancel, progress),
        Format::Xtc | Format::Xtch => write_xtc(job, &chapters, &part, cancel, progress),
    };
    match r {
        Ok(()) => {
            std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
            Ok(dest)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn file_name(m: &Manga, chapters: &[Chapter], ext: &str) -> String {
    let range = match chapters {
        [one] => one.label(),
        [first, .., last] => format!("{} – {}", first.label(), last.label()),
        [] => String::new(),
    };
    let raw = format!("{} - {range}", m.title);
    let mut s: String = raw.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).collect();
    // Keep it well under the usual 255-byte limit (FAT32 on the SD card).
    while s.len() > 150 {
        s.pop();
    }
    format!("{}.{ext}", s.trim())
}

/// Map `f` over `items` on all cores, delivering results in order to `sink`.
/// Works in bounded batches so memory stays small.
fn par_ordered<T: Sync, R: Send>(
    items: &[T],
    cancel: &AtomicBool,
    f: impl Fn(&T) -> Result<R, String> + Sync,
    mut sink: impl FnMut(R) -> Result<(), String>,
) -> Result<(), String> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    for batch in items.chunks(threads) {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let results: Vec<Result<R, String>> = std::thread::scope(|s| {
            let hs: Vec<_> = batch.iter().map(|it| s.spawn(|| f(it))).collect();
            hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("worker panicked".into()))).collect()
        });
        for r in results {
            sink(r?)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------- CBZ

fn write_cbz(
    job: &Job,
    chapters: &[(&Chapter, Vec<PathBuf>)],
    part: &Path,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<(), String> {
    let total: usize = chapters.iter().map(|c| c.1.len()).sum();
    let f = File::create(part).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(BufWriter::new(f));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let ze = |e: zip::result::ZipError| e.to_string();

    zip.start_file("ComicInfo.xml", opts).map_err(ze)?;
    zip.write_all(comic_info(job, total).as_bytes()).map_err(|e| e.to_string())?;

    let mut done = 0;
    for (ci, (ch, pages)) in chapters.iter().enumerate() {
        let folder = format!("{:03} {}", ci + 1, sanitize(&ch.label()));
        par_ordered(pages, cancel, |p| to_jpeg(p), |jpg| {
            done += 1;
            zip.start_file(format!("{folder}/{done:05}.jpg"), opts).map_err(ze)?;
            zip.write_all(&jpg).map_err(|e| e.to_string())?;
            progress(done, total);
            Ok(())
        })?;
    }
    zip.finish().map_err(ze)?.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if matches!(c, '/' | '\\') || c.is_control() { '_' } else { c }).collect()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn comic_info(job: &Job, pages: usize) -> String {
    let m = &job.manga;
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ComicInfo xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\n");
    let chs = &job.chapters;
    let title = if chs.len() == 1 { chs[0].label() } else { format!("{} – {}", chs[0].label(), chs[chs.len() - 1].label()) };
    x += &format!("  <Title>{}</Title>\n  <Series>{}</Series>\n", xml_escape(&title), xml_escape(&m.title));
    if chs.len() == 1 && chs[0].number > 0.0 {
        x += &format!("  <Number>{}</Number>\n", chs[0].number);
    }
    if !m.authors.is_empty() {
        x += &format!("  <Writer>{}</Writer>\n", xml_escape(&m.authors.join(", ")));
    }
    if !m.tags.is_empty() {
        let g: Vec<&str> = m.tags.iter().map(|t| t.title.as_str()).collect();
        x += &format!("  <Genre>{}</Genre>\n", xml_escape(&g.join(", ")));
    }
    x += &format!("  <PageCount>{pages}</PageCount>\n");
    x += if job.opts.rtl { "  <Manga>YesAndRightToLeft</Manga>\n" } else { "  <Manga>Yes</Manga>\n" };
    x += "</ComicInfo>\n";
    x
}

/// Keep JPEGs as they are; re-encode anything else (WebP, PNG, AVIF…) as JPEG,
/// which every comic reader and converter understands.
fn to_jpeg(p: &Path) -> Result<Vec<u8>, String> {
    let data = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok(data);
    }
    let img = image::load_from_memory(&data).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut out = Vec::with_capacity(data.len() * 2);
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
    if is_grey(&img) {
        img.into_luma8().write_with_encoder(enc).map_err(|e| e.to_string())?;
    } else {
        img.into_rgb8().write_with_encoder(enc).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Most manga pages are greyscale stored as RGB; sample to decide.
fn is_grey(img: &image::DynamicImage) -> bool {
    let rgb = match img.as_rgb8() {
        Some(r) => std::borrow::Cow::Borrowed(r),
        None => std::borrow::Cow::Owned(img.to_rgb8()),
    };
    rgb.pixels().step_by(97).all(|p| {
        let [r, g, b] = p.0;
        r.abs_diff(g) < 12 && g.abs_diff(b) < 12 && r.abs_diff(b) < 12
    })
}

// ---------------------------------------------------------------------- XTC

fn write_xtc(
    job: &Job,
    chapters: &[(&Chapter, Vec<PathBuf>)],
    part: &Path,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<(), String> {
    let gray = job.format == Format::Xtch;
    let total: usize = chapters.iter().map(|c| c.1.len()).sum();
    // Pages go to a scratch file first: the index sits before the data and
    // its size depends on how many strips each page is cut into.
    let scratch = part.with_extension("pages");
    let result = (|| {
        let mut data = BufWriter::new(File::create(&scratch).map_err(|e| e.to_string())?);
        let mut out_pages = 0usize;
        let mut toc: Vec<(String, usize, usize)> = Vec::new(); // name, first, last (0-based)
        let mut done = 0;
        for (ch, pages) in chapters {
            let first = out_pages;
            par_ordered(pages, cancel, |p| render_page(p, &job.opts, gray), |rendered| {
                for page in rendered {
                    data.write_all(&page).map_err(|e| e.to_string())?;
                    out_pages += 1;
                }
                done += 1;
                progress(done, total);
                Ok(())
            })?;
            if out_pages > first {
                toc.push((ch.label(), first, out_pages - 1));
            }
        }
        data.flush().map_err(|e| e.to_string())?;
        drop(data);
        if out_pages == 0 {
            return Err("no pages".to_string());
        }
        if out_pages > u16::MAX as usize {
            return Err(format!("{out_pages} pages is more than an XTC file can hold (65535); export fewer chapters"));
        }
        let page_size = page_bytes(gray);
        let mut out = BufWriter::new(File::create(part).map_err(|e| e.to_string())?);
        let author = job.manga.authors.join(", ");
        out.write_all(&container_head(gray, &job.manga.title, &author, &toc, out_pages, page_size))
            .map_err(|e| e.to_string())?;
        let mut src = File::open(&scratch).map_err(|e| e.to_string())?;
        std::io::copy(&mut src, &mut out).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&scratch);
    result
}

const HEADER: usize = 56;
const TITLE: usize = 128;
const AUTHOR: usize = 112;
const TOC_HEAD: usize = 16;
const TOC_ENTRY: usize = 96;
const INDEX_ENTRY: usize = 16;
const PAGE_HEAD: usize = 22;

fn page_bytes(gray: bool) -> usize {
    PAGE_HEAD + if gray { X4_W as usize * X4_H.div_ceil(8) as usize * 2 } else { X4_W.div_ceil(8) as usize * X4_H as usize }
}

fn put_str(buf: &mut [u8], s: &str) {
    // Truncate on a char boundary, always leaving a NUL terminator.
    let mut end = s.len().min(buf.len() - 1);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    buf[..end].copy_from_slice(&s.as_bytes()[..end]);
}

/// Everything before the page data: header, metadata, chapters, index.
fn container_head(gray: bool, title: &str, author: &str, toc: &[(String, usize, usize)], pages: usize, page_size: usize) -> Vec<u8> {
    let meta_off = HEADER;
    let toc_off = HEADER + TITLE + AUTHOR + TOC_HEAD;
    let index_off = toc_off + toc.len() * TOC_ENTRY;
    let data_off = index_off + pages * INDEX_ENTRY;
    let mut b = vec![0u8; data_off];
    b[0..4].copy_from_slice(if gray { b"XTCH" } else { b"XTC\0" });
    b[4..6].copy_from_slice(&1u16.to_le_bytes()); // version 1.0
    b[6..8].copy_from_slice(&(pages as u16).to_le_bytes());
    b[8] = 0; // read direction: pages are already in reading order
    b[9] = 1; // has metadata
    b[10] = 0; // no thumbnails
    b[11] = u8::from(!toc.is_empty()); // has chapters
    b[12..16].copy_from_slice(&1u32.to_le_bytes()); // current page (1-based)
    b[0x10..0x18].copy_from_slice(&(meta_off as u64).to_le_bytes());
    b[0x18..0x20].copy_from_slice(&(index_off as u64).to_le_bytes());
    b[0x20..0x28].copy_from_slice(&(data_off as u64).to_le_bytes());
    // 0x28: thumbnails (none)
    b[0x30..0x38].copy_from_slice(&(toc_off as u64).to_le_bytes());

    put_str(&mut b[meta_off..meta_off + TITLE], title);
    put_str(&mut b[meta_off + TITLE..meta_off + TITLE + AUTHOR], author);
    let th = meta_off + TITLE + AUTHOR;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32);
    b[th..th + 4].copy_from_slice(&now.to_le_bytes());
    b[th + 6..th + 8].copy_from_slice(&(toc.len() as u16).to_le_bytes());

    for (i, (name, first, last)) in toc.iter().enumerate() {
        let e = toc_off + i * TOC_ENTRY;
        put_str(&mut b[e..e + 80], name);
        // Page numbers in the chapter table are 1-based, end inclusive.
        b[e + 80..e + 82].copy_from_slice(&((*first + 1) as u16).to_le_bytes());
        b[e + 82..e + 84].copy_from_slice(&((*last + 1) as u16).to_le_bytes());
    }
    for i in 0..pages {
        let e = index_off + i * INDEX_ENTRY;
        b[e..e + 8].copy_from_slice(&((data_off + i * page_size) as u64).to_le_bytes());
        b[e + 8..e + 12].copy_from_slice(&(page_size as u32).to_le_bytes());
        b[e + 12..e + 14].copy_from_slice(&(X4_W as u16).to_le_bytes());
        b[e + 14..e + 16].copy_from_slice(&(X4_H as u16).to_le_bytes());
    }
    b
}

/// Decode one downloaded page and turn it into one or more encoded
/// XTG/XTH screen pages.
pub fn render_page(p: &Path, o: &XtcOptions, gray: bool) -> Result<Vec<Vec<u8>>, String> {
    let data = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let img = image::load_from_memory(&data).map_err(|e| format!("{}: {e}", p.display()))?;
    drop(data);
    let mut g = img.into_luma8();
    if o.trim {
        g = trim(g);
    }
    auto_levels(&mut g);
    let mut resizer = fr::Resizer::new();
    let mut out = Vec::new();
    for seg in segments(&g, o) {
        let piece = image::imageops::crop_imm(&g, seg.0, seg.1, seg.2, seg.3).to_image();
        let piece = if o.layout == Layout::Portrait { piece } else { image::imageops::rotate90(&piece) };
        let mut screen = fit(&mut resizer, &piece)?;
        let levels: &[u8] = if gray { &[0, 85, 170, 255] } else { &[0, 255] };
        quantize(&mut screen, levels, o.dither);
        out.push(if gray { encode_xth(&screen) } else { encode_xtg(&screen) });
    }
    Ok(out)
}

/// Source rectangles (x, y, w, h) in reading order.
fn segments(g: &GrayImage, o: &XtcOptions) -> Vec<(u32, u32, u32, u32)> {
    let (w, h) = g.dimensions();
    if o.layout == Layout::Portrait {
        return vec![(0, 0, w, h)];
    }
    // Two-page spread: split into single pages first.
    let columns: Vec<(u32, u32)> = if w > h && w >= 2 {
        let mid = w / 2;
        let (left, right) = ((0, mid), (mid, w - mid));
        if o.rtl { vec![right, left] } else { vec![left, right] }
    } else {
        vec![(0, w)]
    };
    let mut out = Vec::new();
    for (x, cw) in columns {
        match o.layout {
            Layout::LandscapeHalves => {
                let hh = h / 2;
                out.push((x, 0, cw, hh.max(1)));
                if h - hh > 0 && hh > 0 {
                    out.push((x, hh, cw, h - hh));
                }
            }
            _ => {
                for (y, sh) in strips(cw, h) {
                    out.push((x, y, cw, sh));
                }
            }
        }
    }
    out
}

/// Overlapping strips of a page column, each with the screen's landscape
/// aspect (800:480) so it fills the display when rotated. At least 5% overlap
/// so no line of text is cut without being repeated; works for tall webtoon
/// pages too.
fn strips(w: u32, h: u32) -> Vec<(u32, u32)> {
    let seg = ((w as u64 * X4_W as u64) / X4_H as u64).max(1) as u32;
    if seg >= h {
        return vec![(0, h)];
    }
    let span = (h - seg) as f64;
    let n = ((span / (seg as f64 * 0.95)).ceil() as u32 + 1).max(2);
    // Normal manga pages (~1:1.42) come out as 3 strips, like XTC.js.
    let shift = span / (n - 1) as f64;
    (0..n)
        .map(|i| {
            let y = (shift * i as f64).round() as u32;
            let y = y.min(h - seg);
            (y, seg)
        })
        .collect()
}

/// Crop near-white margins (scans often have wide borders), keeping a small
/// pad. Never crops more than leaves a sane image.
fn trim(g: GrayImage) -> GrayImage {
    let (w, h) = g.dimensions();
    if w < 32 || h < 32 {
        return g;
    }
    let dark = |px: u8| px < 200;
    let row_ink = |y: u32| (0..w).filter(|&x| dark(g.get_pixel(x, y).0[0])).count() as u32;
    let col_ink = |x: u32, y0: u32, y1: u32| (y0..y1).filter(|&y| dark(g.get_pixel(x, y).0[0])).count() as u32;
    let rt = (w / 200).max(2);
    let Some(top) = (0..h).find(|&y| row_ink(y) > rt) else { return g };
    let bottom = (0..h).rev().find(|&y| row_ink(y) > rt).unwrap_or(h - 1);
    let ct = ((bottom - top) / 200).max(2);
    let left = (0..w).find(|&x| col_ink(x, top, bottom + 1) > ct).unwrap_or(0);
    let right = (0..w).rev().find(|&x| col_ink(x, top, bottom + 1) > ct).unwrap_or(w - 1);
    let pad = (w.max(h) / 100).max(2);
    let (x0, y0) = (left.saturating_sub(pad), top.saturating_sub(pad));
    let (x1, y1) = ((right + pad).min(w - 1), (bottom + pad).min(h - 1));
    let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
    // Don't trust a crop that throws away most of the page (e.g. a mostly
    // white page with one small panel): keep at least 60% each way.
    if cw < w * 3 / 5 || ch < h * 3 / 5 {
        return g;
    }
    image::imageops::crop_imm(&g, x0, y0, cw, ch).to_image()
}

/// Stretch so the darkest 0.5% is black and the lightest 0.5% white. Grey,
/// washed-out scans become crisp; clean black-and-white pages are unchanged.
fn auto_levels(g: &mut GrayImage) {
    let mut hist = [0u64; 256];
    for p in g.pixels() {
        hist[p.0[0] as usize] += 1;
    }
    let n: u64 = hist.iter().sum();
    let cut = n / 200;
    let (mut lo, mut acc) = (0usize, 0u64);
    while lo < 255 && acc + hist[lo] <= cut {
        acc += hist[lo];
        lo += 1;
    }
    let (mut hi, mut acc) = (255usize, 0u64);
    while hi > 0 && acc + hist[hi] <= cut {
        acc += hist[hi];
        hi -= 1;
    }
    if hi <= lo + 64 || (lo == 0 && hi == 255) {
        return;
    }
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        *v = (((i as i32 - lo as i32) * 255) / (hi as i32 - lo as i32)).clamp(0, 255) as u8;
    }
    for p in g.pixels_mut() {
        p.0[0] = lut[p.0[0] as usize];
    }
}

/// Scale to fit 480×800 keeping the aspect ratio, centred on white.
fn fit(resizer: &mut fr::Resizer, src: &GrayImage) -> Result<GrayImage, String> {
    let (w, h) = src.dimensions();
    let scale = (X4_W as f64 / w as f64).min(X4_H as f64 / h as f64);
    let nw = ((w as f64 * scale).round() as u32).clamp(1, X4_W);
    let nh = ((h as f64 * scale).round() as u32).clamp(1, X4_H);
    let s = fr::images::Image::from_vec_u8(w, h, src.as_raw().clone(), fr::PixelType::U8).map_err(|e| e.to_string())?;
    let mut d = fr::images::Image::new(nw, nh, fr::PixelType::U8);
    let opts = fr::ResizeOptions::new().resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3));
    resizer.resize(&s, &mut d, &opts).map_err(|e| e.to_string())?;
    let scaled = GrayImage::from_raw(nw, nh, d.into_vec()).ok_or("resize")?;
    let mut screen = GrayImage::from_pixel(X4_W, X4_H, image::Luma([255]));
    image::imageops::replace(&mut screen, &scaled, ((X4_W - nw) / 2) as i64, ((X4_H - nh) / 2) as i64);
    Ok(screen)
}

/// Reduce to the given grey levels, with Floyd–Steinberg error diffusion
/// (serpentine) when `dither` is set.
fn quantize(g: &mut GrayImage, levels: &[u8], dither: bool) {
    let nearest = |v: i32| -> u8 {
        let mut best = levels[0];
        for &l in levels {
            if (v - l as i32).abs() < (v - best as i32).abs() {
                best = l;
            }
        }
        best
    };
    let (w, h) = (g.width() as usize, g.height() as usize);
    let buf: &mut [u8] = &mut **g;
    if !dither {
        for p in buf.iter_mut() {
            *p = nearest(*p as i32);
        }
        return;
    }
    // Error rows in 1/16ths, with one pixel of padding each side.
    let mut cur = vec![0i32; w + 2];
    let mut next = vec![0i32; w + 2];
    for y in 0..h {
        let ltr = y % 2 == 0;
        for i in 0..w {
            let x = if ltr { i } else { w - 1 - i };
            let idx = y * w + x;
            let v = (buf[idx] as i32 + cur[x + 1] / 16).clamp(0, 255);
            let q = nearest(v);
            buf[idx] = q;
            let err = v - q as i32;
            let (fwd, back) = if ltr { (x + 2, x) } else { (x, x + 2) };
            cur[fwd] += err * 7;
            next[back] += err * 3;
            next[x + 1] += err * 5;
            next[fwd] += err;
        }
        std::mem::swap(&mut cur, &mut next);
        next.iter_mut().for_each(|e| *e = 0);
    }
}

fn page_header(magic: &[u8; 4], w: u32, h: u32, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(PAGE_HEAD + data.len());
    b.extend_from_slice(magic);
    b.extend_from_slice(&(w as u16).to_le_bytes());
    b.extend_from_slice(&(h as u16).to_le_bytes());
    b.push(0); // colour mode
    b.push(0); // compression
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    // "md5" field: optional; xtcjs stores the first 8 data bytes, so do the same.
    let mut d = [0u8; 8];
    let n = data.len().min(8);
    d[..n].copy_from_slice(&data[..n]);
    b.extend_from_slice(&d);
    b.extend_from_slice(data);
    b
}

/// XTG: 1 bit per pixel, rows top to bottom, MSB = leftmost, 1 = white.
fn encode_xtg(g: &GrayImage) -> Vec<u8> {
    let (w, h) = g.dimensions();
    let row = w.div_ceil(8) as usize;
    let mut data = vec![0u8; row * h as usize];
    for (x, y, p) in g.enumerate_pixels() {
        if p.0[0] >= 128 {
            data[y as usize * row + x as usize / 8] |= 0x80 >> (x % 8);
        }
    }
    page_header(b"XTG\0", w, h, &data)
}

/// XTH: 2 bits per pixel in two planes; columns right to left, 8 vertical
/// pixels per byte (MSB = top). Device values: 0 white, 1 dark grey,
/// 2 light grey, 3 black; plane 1 holds the high bit.
fn encode_xth(g: &GrayImage) -> Vec<u8> {
    let (w, h) = g.dimensions();
    let col = h.div_ceil(8) as usize;
    let plane = col * w as usize;
    let mut data = vec![0u8; plane * 2];
    for (x, y, p) in g.enumerate_pixels() {
        let pv: u8 = match p.0[0] {
            0..=42 => 3,    // black
            43..=127 => 1,  // dark grey
            128..=212 => 2, // light grey
            _ => 0,         // white
        };
        let i = (w - 1 - x) as usize * col + y as usize / 8;
        let bit = 0x80 >> (y % 8);
        if pv & 2 != 0 {
            data[i] |= bit;
        }
        if pv & 1 != 0 {
            data[plane + i] |= bit;
        }
    }
    page_header(b"XTH\0", w, h, &data)
}

// ------------------------------------------------------------- reading back

/// Minimal XTC/XTCH reader for tests: returns (pages, chapters, page images).
#[allow(dead_code)]
pub fn decode_xtc(path: &Path) -> Result<(usize, Vec<(String, u16, u16)>, Vec<GrayImage>), String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut b = Vec::new();
    f.read_to_end(&mut b).map_err(|e| e.to_string())?;
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let gray = match &b[0..4] {
        b"XTCH" => true,
        b"XTC\0" => false,
        _ => return Err("bad magic".into()),
    };
    let pages = u16_at(6) as usize;
    let index = u64_at(0x18) as usize;
    let mut chapters = Vec::new();
    if b[11] == 1 {
        let toc = u64_at(0x30) as usize;
        let mut o = toc;
        while o + TOC_ENTRY <= index {
            let name = String::from_utf8_lossy(&b[o..o + 80]).trim_end_matches('\0').to_string();
            chapters.push((name, u16_at(o + 80), u16_at(o + 82)));
            o += TOC_ENTRY;
        }
    }
    let mut imgs = Vec::new();
    for i in 0..pages {
        let e = index + i * INDEX_ENTRY;
        let off = u64_at(e) as usize;
        let magic = &b[off..off + 4];
        let (w, h) = (u16_at(off + 4) as u32, u16_at(off + 6) as u32);
        let size = u32_at(off + 10) as usize;
        let d = &b[off + PAGE_HEAD..off + PAGE_HEAD + size];
        let mut img = GrayImage::new(w, h);
        if gray {
            if magic != b"XTH\0" {
                return Err(format!("page {i}: expected XTH"));
            }
            let col = h.div_ceil(8) as usize;
            let plane = col * w as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (w - 1 - x) as usize * col + y as usize / 8;
                    let bit = 7 - (y % 8);
                    let pv = (((d[idx] >> bit) & 1) << 1) | ((d[plane + idx] >> bit) & 1);
                    img.put_pixel(x, y, image::Luma([[255, 85, 170, 0][pv as usize]]));
                }
            }
        } else {
            if magic != b"XTG\0" {
                return Err(format!("page {i}: expected XTG"));
            }
            let row = w.div_ceil(8) as usize;
            for y in 0..h {
                for x in 0..w {
                    let on = d[y as usize * row + x as usize / 8] & (0x80 >> (x % 8)) != 0;
                    img.put_pixel(x, y, image::Luma([if on { 255 } else { 0 }]));
                }
            }
        }
        imgs.push(img);
    }
    Ok((pages, chapters, imgs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_normal_page_is_three() {
        let s = strips(1350, 1920);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].0, 0);
        assert_eq!(s[2].0 + s[2].1, 1920);
        // overlap between consecutive strips
        assert!(s[1].0 < s[0].0 + s[0].1);
    }

    #[test]
    fn strips_webtoon_covers_everything() {
        let s = strips(800, 12000);
        assert!(s.len() > 20);
        for p in s.windows(2) {
            assert!(p[1].0 <= p[0].0 + p[0].1, "gap between strips");
        }
        assert_eq!(s.last().map(|l| l.0 + l.1), Some(12000));
    }

    #[test]
    fn spread_is_split_right_first() {
        let g = GrayImage::from_pixel(2000, 1400, image::Luma([255]));
        let segs = segments(&g, &XtcOptions { layout: Layout::LandscapeHalves, ..Default::default() });
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].0, 1000); // right page first for manga
    }

    #[test]
    fn page_encodings_roundtrip() {
        let mut g = GrayImage::from_pixel(X4_W, X4_H, image::Luma([255]));
        for y in 0..X4_H {
            for x in 0..X4_W {
                let v = [0u8, 85, 170, 255][((x / 120) as usize) % 4];
                g.put_pixel(x, y, image::Luma([if y < 10 { 0 } else { v }]));
            }
        }
        let xth = encode_xth(&g);
        assert_eq!(xth.len(), page_bytes(true));
        let xtg = encode_xtg(&g);
        assert_eq!(xtg.len(), page_bytes(false));

        let dir = std::env::temp_dir().join(format!("kd-xtc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for gray in [false, true] {
            let page = if gray { xth.clone() } else { xtg.clone() };
            let toc = vec![("Chapter 1".to_string(), 0, 0), ("Chapter 2 with a very long name that goes past the eighty byte limit of the field".to_string(), 1, 1)];
            let mut file = container_head(gray, "Test", "Me", &toc, 2, page.len());
            file.extend_from_slice(&page);
            file.extend_from_slice(&page);
            let p = dir.join(if gray { "t.xtch" } else { "t.xtc" });
            std::fs::write(&p, &file).unwrap();
            let (n, chs, imgs) = decode_xtc(&p).unwrap();
            assert_eq!(n, 2);
            assert_eq!(chs.len(), 2);
            assert_eq!((chs[0].1, chs[0].2), (1, 1));
            assert_eq!((chs[1].1, chs[1].2), (2, 2));
            assert!(chs[1].0.len() <= 79);
            // Title is where CrossPoint reads it (0x38), author at 0xB8.
            assert_eq!(&file[0x38..0x3C], b"Test");
            assert_eq!(&file[0xB8..0xBA], b"Me");
            let img = &imgs[1];
            assert_eq!(img.get_pixel(5, 5).0[0], 0);
            if gray {
                for (i, want) in [0u8, 85, 170, 255].iter().enumerate() {
                    assert_eq!(img.get_pixel(i as u32 * 120 + 60, 400).0[0], *want);
                }
            } else {
                assert_eq!(img.get_pixel(60, 400).0[0], 0);
                assert_eq!(img.get_pixel(420, 400).0[0], 255);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dither_keeps_mean_brightness() {
        let mut g = GrayImage::from_pixel(200, 200, image::Luma([100]));
        quantize(&mut g, &[0, 255], true);
        let mean: f64 = g.pixels().map(|p| p.0[0] as f64).sum::<f64>() / 40000.0;
        assert!((mean - 100.0).abs() < 5.0, "mean {mean}");
    }

    /// End-to-end on real downloads: cargo test export_real_download -- --ignored --nocapture
    /// (writes to $KD_EXPORT_DIR or /tmp/kd-export)
    #[test]
    #[ignore]
    fn export_real_download() {
        let Some(m) = downloads::downloaded_manga().into_iter().next() else { return };
        let dir = downloads::manga_dir(&m);
        let mut chapters = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            if let Ok(id) = e.file_name().to_string_lossy().parse::<i64>() {
                if downloads::is_downloaded(&m, id) {
                    chapters.push(Chapter { id, title: Some(format!("Chapter {id}")), ..Default::default() });
                }
            }
        }
        let out = std::env::var("KD_EXPORT_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir().join("kd-export"));
        for format in [Format::Cbz, Format::Xtc, Format::Xtch] {
            let job = Job { manga: m.clone(), chapters: chapters.clone(), dir: out.clone(), format, opts: XtcOptions::default() };
            let t = std::time::Instant::now();
            let p = run(&job, &AtomicBool::new(false), &|_, _| {}).unwrap();
            eprintln!("{format:?}: {} ({} bytes) in {:?}", p.display(), std::fs::metadata(&p).unwrap().len(), t.elapsed());
            if format != Format::Cbz {
                let (n, chs, imgs) = decode_xtc(&p).unwrap();
                eprintln!("  {n} pages, chapters {chs:?}");
                for (i, img) in imgs.iter().take(4).enumerate() {
                    img.save(out.join(format!("preview-{}-{i}.png", format.ext()))).unwrap();
                }
            }
        }
    }
}
