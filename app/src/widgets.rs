//! Shared widgets: virtualised cover grid, cover rows, small helpers.

use crate::images::{paint_tiles, Images, Img, ImgSrc};
use crate::model::Manga;
use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, Ui};

pub const CARD_W: f32 = 150.0;
pub const COVER_RATIO: f32 = 1.45;
const TEXT_H: f32 = 38.0;
const GAP: f32 = 14.0;

pub const BOOK: &str = "_BOOK";
pub const WEB_BOOK: &str = "_GUTENBERG";

pub fn cover_key(m: &Manga) -> String {
    match m.source.as_str() {
        BOOK => format!("b:{}:{}", m.url, m.cover.as_deref().unwrap_or("")),
        _ => format!("c:{}", m.cover.as_deref().unwrap_or("")),
    }
}

/// Where a card's cover comes from: a parser source, an EPUB entry, or plain web mirrors.
pub fn cover_src(m: &Manga, url: String) -> ImgSrc {
    match m.source.as_str() {
        BOOK => ImgSrc::Zip { file: m.url.clone().into(), entry: url },
        WEB_BOOK => ImgSrc::Web { urls: crate::books::mirrors(&url) },
        _ => ImgSrc::Url { source: m.source.clone(), url },
    }
}

/// Paint a cover image (cropped to fill) into `rect`.
pub fn paint_cover(ui: &Ui, images: &mut Images, m: &Manga, rect: Rect) {
    let rounding = CornerRadius::same(6);
    ui.painter().rect_filled(rect, rounding, Color32::from_gray(38));
    let Some(url) = m.cover.clone() else {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, "No cover", FontId::proportional(12.0), Color32::GRAY);
        return;
    };
    let ppp = ui.ctx().pixels_per_point();
    let want = (rect.width() * ppp).ceil() as u32;
    match images.get(&cover_key(m), || cover_src(m, url), want) {
        Img::Ready { tiles, size } if tiles.len() == 1 => {
            // object-fit: cover
            let (w, h) = (size[0] as f32, size[1] as f32);
            let target = rect.width() / rect.height();
            let actual = w / h;
            let uv = if actual > target {
                let f = target / actual;
                Rect::from_min_max(pos2((1.0 - f) / 2.0, 0.0), pos2((1.0 + f) / 2.0, 1.0))
            } else {
                let f = actual / target;
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, f))
            };
            egui::Image::new((tiles[0].tex.id(), rect.size()))
                .uv(uv)
                .corner_radius(rounding)
                .paint_at(ui, rect);
        }
        Img::Ready { tiles, size } => paint_tiles(ui, rect, tiles, size),
        Img::Loading => {}
        Img::Failed(_) => {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, "✖", FontId::proportional(18.0), Color32::GRAY);
        }
    }
}

pub struct GridOut {
    pub clicked: Option<usize>,
    pub secondary: Option<usize>,
    pub near_end: bool,
}

/// Virtualised grid: only visible rows are laid out and painted.
pub fn manga_grid(
    ui: &mut Ui,
    images: &mut Images,
    id: impl std::hash::Hash + std::fmt::Debug,
    items: &[Manga],
    badge: &dyn Fn(usize) -> Option<String>,
) -> GridOut {
    let mut out = GridOut { clicked: None, secondary: None, near_end: false };
    let avail = ui.available_width();
    let cols = ((avail + GAP) / (CARD_W + GAP)).floor().max(1.0) as usize;
    let card_w = ((avail + GAP) / cols as f32 - GAP).min(CARD_W * 1.35);
    let cover_h = card_w * COVER_RATIO;
    let row_h = cover_h + TEXT_H + GAP;
    let rows = items.len().div_ceil(cols);
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show_viewport(ui, |ui, vp| {
        ui.set_height(rows as f32 * row_h + 40.0);
        let origin = ui.min_rect().min;
        let first = (vp.min.y / row_h).floor().max(0.0) as usize;
        let last = ((vp.max.y / row_h).ceil() as usize + 1).min(rows);
        // Prefetch one row ahead in both directions
        for r in [first.saturating_sub(1), last] {
            for c in 0..cols {
                let i = r * cols + c;
                if let Some(m) = items.get(i) {
                    if let Some(url) = m.cover.clone() {
                        let w = (card_w * ui.ctx().pixels_per_point()) as u32;
                        images.prefetch(&cover_key(m), || cover_src(m, url), w);
                    }
                }
            }
        }
        for r in first..last {
            for c in 0..cols {
                let i = r * cols + c;
                let Some(m) = items.get(i) else { break };
                let x = origin.x + c as f32 * (card_w + GAP);
                let y = origin.y + r as f32 * row_h;
                let cover = Rect::from_min_size(pos2(x, y), vec2(card_w, cover_h));
                let full = Rect::from_min_size(pos2(x, y), vec2(card_w, cover_h + TEXT_H));
                let resp = ui.interact(full, ui.scope_id().with(("card", i)), Sense::click());
                paint_cover(ui, images, m, cover);
                if resp.hovered() {
                    ui.painter().rect_stroke(cover, CornerRadius::same(6), Stroke::new(2.0, ui.visuals().selection.bg_fill), egui::StrokeKind::Outside);
                }
                if let Some(b) = badge(i) {
                    let r = Rect::from_min_size(pos2(cover.right() - 34.0, cover.top() + 4.0), vec2(30.0, 18.0));
                    ui.painter().rect_filled(r, CornerRadius::same(9), Color32::from_rgb(200, 60, 60));
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, b, FontId::proportional(11.0), Color32::WHITE);
                }
                let galley = ui.painter().layout(
                    m.title.clone(),
                    FontId::proportional(13.0),
                    ui.visuals().text_color(),
                    card_w,
                );
                let text_rect = Rect::from_min_size(pos2(x, cover.bottom() + 4.0), vec2(card_w, TEXT_H - 4.0));
                ui.painter().with_clip_rect(text_rect).galley(text_rect.min, galley, Color32::WHITE);
                if resp.clicked() {
                    out.clicked = Some(i);
                }
                if resp.secondary_clicked() {
                    out.secondary = Some(i);
                }
                resp.on_hover_text(&m.title);
            }
        }
        out.near_end = last + 2 >= rows;
    });
    out
}

/// Horizontal strip of covers (for global search results).
pub fn cover_row(ui: &mut Ui, images: &mut Images, id: impl std::hash::Hash + std::fmt::Debug, items: &[Manga]) -> Option<usize> {
    let mut clicked = None;
    let w = 110.0;
    let h = w * COVER_RATIO;
    egui::ScrollArea::horizontal().id_salt(id).show_viewport(ui, |ui, vp| {
        let step = w + 10.0;
        ui.set_width(items.len() as f32 * step);
        ui.set_height(h + 34.0);
        let origin = ui.min_rect().min;
        let first = (vp.min.x / step).floor().max(0.0) as usize;
        let last = ((vp.max.x / step).ceil() as usize + 1).min(items.len());
        for i in first..last {
            let m = &items[i];
            let x = origin.x + i as f32 * step;
            let cover = Rect::from_min_size(pos2(x, origin.y), vec2(w, h));
            let full = Rect::from_min_size(cover.min, vec2(w, h + 34.0));
            let resp = ui.interact(full, ui.scope_id().with(("row", i)), Sense::click());
            paint_cover(ui, images, m, cover);
            let galley = ui.painter().layout(m.title.clone(), FontId::proportional(12.0), ui.visuals().text_color(), w);
            let tr = Rect::from_min_size(pos2(x, cover.bottom() + 3.0), vec2(w, 30.0));
            ui.painter().with_clip_rect(tr).galley(tr.min, galley, Color32::WHITE);
            if resp.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

pub fn time_ago(ts: i64) -> String {
    let d = crate::store::now() - ts;
    match d {
        i64::MIN..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", d / 60),
        3600..=86399 => format!("{}h ago", d / 3600),
        _ => format!("{}d ago", d / 86400),
    }
}

pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

pub fn error_box(ui: &mut Ui, msg: &str) -> bool {
    let mut retry = false;
    ui.vertical_centered(|ui| {
        ui.add_space(30.0);
        ui.colored_label(Color32::from_rgb(230, 110, 110), msg);
        if ui.button("Retry").clicked() {
            retry = true;
        }
    });
    retry
}

pub fn spinner(ui: &mut Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(30.0);
        ui.spinner();
    });
}
