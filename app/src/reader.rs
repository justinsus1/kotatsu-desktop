//! The reader: webtoon (continuous vertical scroll, virtualised) and paged
//! (single page, LTR/RTL) modes, with preloading and progress tracking.

use crate::app::{App, Nav};
use crate::bridge::Pending;
use crate::downloads;
use crate::images::{paint_tiles, Img, ImgSrc};
use crate::model::{Chapter, Manga, Page};
use crate::store::{ReaderMode, Store};
use egui::{pos2, vec2, Align2, Color32, FontId, Key, Rect, RichText, Sense, Ui};
use serde_json::json;
use std::path::PathBuf;

const DEFAULT_ASPECT: f32 = 1.42;

pub struct ReaderState {
    manga: Manga,
    ch: usize,
    pages: Vec<Page>,
    local: Option<Vec<PathBuf>>,
    pages_p: Option<Pending>,
    error: Option<String>,
    page: usize,
    start_page: usize,
    mode: ReaderMode,
    aspects: Vec<Option<f32>>,
    tops: Vec<f32>,
    scroll_y: f32,
    jump_page: Option<usize>,
    show_bar: bool,
    fullscreen: bool,
    recorded: Option<(usize, usize)>,
    next_pages: Option<(usize, Pending)>,
    wheel_acc: f32,
    loaded: bool,
}

impl ReaderState {
    pub fn new(manga: Manga, ch: usize, page: usize, mode: ReaderMode) -> Self {
        ReaderState {
            manga,
            ch,
            pages: vec![],
            local: None,
            pages_p: None,
            error: None,
            page,
            start_page: page,
            mode,
            aspects: vec![],
            tops: vec![],
            scroll_y: 0.0,
            jump_page: None,
            show_bar: true,
            fullscreen: false,
            recorded: None,
            next_pages: None,
            wheel_acc: 0.0,
            loaded: false,
        }
    }

    fn chapters(&self) -> &[Chapter] {
        self.manga.chapters.as_deref().unwrap_or(&[])
    }

    fn chapter(&self) -> Option<&Chapter> {
        self.chapters().get(self.ch)
    }

    /// Next/previous chapter in the same translation branch.
    fn sibling(&self, forward: bool) -> Option<usize> {
        let chs = self.chapters();
        let branch = &chs.get(self.ch)?.branch;
        if forward {
            (self.ch + 1..chs.len()).find(|&i| &chs[i].branch == branch)
        } else {
            (0..self.ch).rev().find(|&i| &chs[i].branch == branch)
        }
    }

    fn page_count(&self) -> usize {
        self.local.as_ref().map_or(self.pages.len(), Vec::len)
    }

    fn page_key(&self, i: usize) -> String {
        match &self.local {
            Some(files) => format!("f:{}", files[i].display()),
            None => format!("p:{}:{}", self.manga.source, self.pages[i].url),
        }
    }

    fn page_src(&self, i: usize) -> ImgSrc {
        match &self.local {
            Some(files) => ImgSrc::File(files[i].clone()),
            None => ImgSrc::Page { source: self.manga.source.clone(), page: self.pages[i].clone() },
        }
    }

    fn goto_chapter(&mut self, ch: usize, page: usize) {
        if ch >= self.chapters().len() {
            return;
        }
        let pre = self.next_pages.take().filter(|(c, _)| *c == ch);
        *self = ReaderState {
            manga: std::mem::take(&mut self.manga),
            mode: self.mode,
            fullscreen: self.fullscreen,
            show_bar: self.show_bar,
            ..ReaderState::new(Manga::default(), ch, page, self.mode)
        };
        if let Some((_, p)) = pre {
            self.pages_p = Some(p);
        }
    }

    pub fn on_leave_check(&mut self, _store: &mut Store) {}
}

impl App {
    pub fn reader_ui(&mut self, ui: &mut Ui, st: &mut ReaderState) -> Nav {
        let mut nav = Nav::None;
        let ctx = ui.ctx().clone();
        ui.painter().rect_filled(ui.max_rect(), 0.0, Color32::BLACK);

        // ---- load pages
        if !st.loaded && st.pages_p.is_none() && st.error.is_none() {
            let Some(ch) = st.chapter().cloned() else { return Nav::Back };
            if let Some(files) = downloads::local_pages(&st.manga, ch.id) {
                st.local = Some(files);
                st.loaded = true;
            } else {
                st.pages_p = Some(self.bridge.call("pages", json!({"source": st.manga.source, "chapter": ch})));
            }
        }
        if let Some(r) = st.pages_p.as_ref().and_then(|p| p.get::<Vec<Page>>()) {
            st.pages_p = None;
            match r {
                Ok(p) => {
                    st.pages = p;
                    st.loaded = true;
                }
                Err(e) => st.error = Some(e),
            }
        }
        if st.loaded && st.aspects.len() != st.page_count() {
            st.aspects = vec![None; st.page_count()];
            st.page = st.start_page.min(st.page_count().saturating_sub(1));
            st.jump_page = Some(st.page);
        }

        // ---- keys
        let typing = ctx.memory(|m| m.focused().is_some());
        let (esc, k_next_ch, k_prev_ch, k_mode, k_full, k_bar) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape) || i.key_pressed(Key::Backspace) || i.pointer.button_pressed(egui::PointerButton::Extra1),
                i.key_pressed(Key::N) || i.key_pressed(Key::CloseBracket),
                i.key_pressed(Key::P) || i.key_pressed(Key::OpenBracket),
                i.key_pressed(Key::M),
                i.key_pressed(Key::F) || i.key_pressed(Key::F11),
                i.key_pressed(Key::Tab) || i.key_pressed(Key::H),
            )
        });
        if !typing {
            if esc {
                if st.fullscreen {
                    st.fullscreen = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                } else {
                    self.save_progress(st);
                    return Nav::Back;
                }
            }
            if k_full {
                st.fullscreen = !st.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(st.fullscreen));
            }
            if k_bar {
                st.show_bar = !st.show_bar;
            }
            if k_mode {
                st.mode = match st.mode {
                    ReaderMode::Webtoon => ReaderMode::Paged,
                    ReaderMode::Paged => ReaderMode::PagedRtl,
                    ReaderMode::PagedRtl => ReaderMode::Webtoon,
                };
                st.jump_page = Some(st.page);
            }
            if k_next_ch {
                self.next_chapter(st);
            }
            if k_prev_ch {
                if let Some(p) = st.sibling(false) {
                    self.save_progress(st);
                    st.goto_chapter(p, 0);
                }
            }
        }

        // ---- top bar
        if st.show_bar {
            egui::Panel::top("reader-bar").frame(egui::Frame::NONE.fill(Color32::from_black_alpha(230)).inner_margin(6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("←").clicked() {
                        self.save_progress(st);
                        nav = Nav::Back;
                    }
                    ui.label(RichText::new(&st.manga.title).strong());
                    let branch = st.chapter().and_then(|c| c.branch.clone());
                    let mut sel = st.ch;
                    let cur = st.chapter().map(Chapter::label).unwrap_or_default();
                    egui::ComboBox::from_id_salt("ch").width(220.0).selected_text(cur).show_ui(ui, |ui| {
                        for (i, c) in st.chapters().iter().enumerate().rev() {
                            if c.branch == branch {
                                ui.selectable_value(&mut sel, i, c.label());
                            }
                        }
                    });
                    if sel != st.ch {
                        self.save_progress(st);
                        st.goto_chapter(sel, 0);
                    }
                    let prev = st.sibling(false);
                    if ui.add_enabled(prev.is_some(), egui::Button::new("◀")).on_hover_text("Previous chapter (P)").clicked() {
                        self.save_progress(st);
                        st.goto_chapter(prev.unwrap_or(0), 0);
                    }
                    if ui.add_enabled(st.sibling(true).is_some(), egui::Button::new("▶")).on_hover_text("Next chapter (N)").clicked() {
                        self.next_chapter(st);
                    }
                    if st.loaded {
                        ui.label(format!("{}/{}", st.page + 1, st.page_count()));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(if st.fullscreen { "Exit fullscreen" } else { "Fullscreen" }).on_hover_text("Fullscreen (F)").clicked() {
                            st.fullscreen = !st.fullscreen;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(st.fullscreen));
                        }
                        let ml = match st.mode {
                            ReaderMode::Webtoon => "Webtoon",
                            ReaderMode::Paged => "Paged →",
                            ReaderMode::PagedRtl => "Paged ←",
                        };
                        if ui.button(ml).on_hover_text("Reader mode (M)").clicked() {
                            st.mode = match st.mode {
                                ReaderMode::Webtoon => ReaderMode::Paged,
                                ReaderMode::Paged => ReaderMode::PagedRtl,
                                ReaderMode::PagedRtl => ReaderMode::Webtoon,
                            };
                            st.jump_page = Some(st.page);
                        }
                        if st.local.is_some() {
                            ui.label(RichText::new("offline").color(Color32::from_rgb(120, 180, 120)));
                        }
                    });
                });
            });
        }

        if !matches!(nav, Nav::None) {
            return nav;
        }
        if let Some(e) = st.error.clone() {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.colored_label(Color32::from_rgb(230, 110, 110), &e);
                if ui.button("Retry").clicked() {
                    st.error = None;
                }
            });
            return nav;
        }
        if !st.loaded {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.spinner();
            });
            return nav;
        }
        if st.page_count() == 0 {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.label("This chapter has no pages.");
            });
            return nav;
        }

        match st.mode {
            ReaderMode::Webtoon => self.webtoon(ui, st, typing),
            ReaderMode::Paged | ReaderMode::PagedRtl => self.paged(ui, st, typing),
        }

        // ---- progress & next chapter prefetch
        let n = st.page_count();
        if st.recorded != Some((st.ch, st.page)) {
            st.recorded = Some((st.ch, st.page));
            self.save_progress(st);
        }
        if let Some(next) = st.sibling(true).filter(|_| st.page + 3 >= n && st.next_pages.is_none()) {
            let ch = st.chapters()[next].clone();
            if !downloads::is_downloaded(&st.manga, ch.id) {
                st.next_pages = Some((next, self.bridge.call("pages", json!({"source": st.manga.source, "chapter": ch}))));
            }
        }
        nav
    }

    fn save_progress(&mut self, st: &ReaderState) {
        let Some(ch) = st.chapter().cloned() else { return };
        if !st.loaded {
            return;
        }
        let chs = st.chapters();
        let n = st.page_count().max(1);
        let percent = if chs.is_empty() { 0.0 } else { ((st.ch as f32) + (st.page + 1) as f32 / n as f32) / chs.len() as f32 };
        self.store.record(&st.manga, ch.id, &ch.label(), st.page, percent.clamp(0.0, 1.0), chs.len() as i32);
        if st.page + 1 >= st.page_count() {
            self.store.mark_read(&st.manga, ch.id);
        }
    }

    fn next_chapter(&mut self, st: &mut ReaderState) {
        if let Some(next) = st.sibling(true) {
            if let Some(ch) = st.chapter().cloned() {
                self.store.mark_read(&st.manga, ch.id);
            }
            self.save_progress(st);
            st.goto_chapter(next, 0);
        } else {
            self.toast("This is the last chapter");
        }
    }

    fn webtoon(&mut self, ui: &mut Ui, st: &mut ReaderState, typing: bool) {
        let avail = ui.available_size();
        let ppp = ui.ctx().pixels_per_point();
        let w = avail.x.min(self.store.settings.reader_width).floor();
        let gap = self.store.settings.reader_gap;
        let n = st.page_count();

        // Layout from known aspect ratios
        let mut tops = Vec::with_capacity(n + 1);
        let mut y = 0.0;
        for a in &st.aspects {
            tops.push(y);
            y += w * a.unwrap_or(DEFAULT_ASPECT) + gap;
        }
        tops.push(y);
        let total = y;

        // Keep the page under the top edge in place when heights above it change
        let mut set_offset = None;
        if let Some(p) = st.jump_page.take() {
            set_offset = Some(tops[p.min(n - 1)]);
        } else if st.tops.len() == tops.len() && st.tops != tops {
            let a = st.tops.partition_point(|&t| t <= st.scroll_y).saturating_sub(1).min(n - 1);
            let old_h = (st.tops[a + 1] - st.tops[a]).max(1.0);
            let new_h = tops[a + 1] - tops[a];
            let ny = tops[a] + (st.scroll_y - st.tops[a]) * new_h / old_h;
            if (ny - st.scroll_y).abs() > 0.5 {
                set_offset = Some(ny);
            }
        }
        st.tops = tops.clone();

        let (k_down, k_up, k_pgdn, k_pgup, k_home, k_end) = if typing {
            Default::default()
        } else {
            ui.input(|i| {
                let shift = i.modifiers.shift;
                (
                    i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::J) || i.key_pressed(Key::S),
                    i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::K) || i.key_pressed(Key::W),
                    i.key_pressed(Key::PageDown) || (i.key_pressed(Key::Space) && !shift) || i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::D),
                    i.key_pressed(Key::PageUp) || (i.key_pressed(Key::Space) && shift) || i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::A),
                    i.key_pressed(Key::Home),
                    i.key_pressed(Key::End),
                )
            })
        };
        if k_home {
            set_offset = Some(0.0);
        }
        if k_end {
            set_offset = Some(total);
        }

        let mut area = egui::ScrollArea::vertical().id_salt(("webtoon", st.ch)).auto_shrink([false, false]).animated(true);
        if let Some(o) = set_offset {
            area = area.vertical_scroll_offset(o.max(0.0));
        }
        let preload = self.store.settings.preload_pages;
        let mut want_next = false;
        let out = area.show_viewport(ui, |ui, vp| {
            let end_h = 160.0;
            ui.set_height(total + end_h);
            ui.set_width(avail.x);
            let origin = ui.min_rect().min;
            let x0 = origin.x + ((avail.x - w) / 2.0).floor();

            let step = vp.height() * 0.9;
            let delta = if k_down { 140.0 } else if k_up { -140.0 } else if k_pgdn { step } else if k_pgup { -step } else { 0.0 };
            if delta != 0.0 {
                ui.scroll_with_delta(vec2(0.0, -delta));
            }

            let first = tops.partition_point(|&t| t <= vp.min.y).saturating_sub(1);
            let last = tops.partition_point(|&t| t < vp.max.y).min(n);
            let probe = vp.min.y + vp.height() * 0.35;
            st.page = tops.partition_point(|&t| t <= probe).saturating_sub(1).min(n - 1);
            let want_w = (w * ppp).ceil() as u32;

            for i in first..last {
                let rect = Rect::from_min_size(pos2(x0, origin.y + tops[i]), vec2(w, tops[i + 1] - tops[i] - gap));
                let key = st.page_key(i);
                let src = st.page_src(i);
                match self.images.get(&key, || src, want_w) {
                    Img::Ready { tiles, size } => {
                        let a = size[1] as f32 / size[0].max(1) as f32;
                        if st.aspects[i] != Some(a) {
                            st.aspects[i] = Some(a);
                            ui.ctx().request_repaint();
                        }
                        paint_tiles(ui, rect, tiles, size);
                    }
                    Img::Loading => {
                        ui.painter().text(rect.center(), Align2::CENTER_CENTER, format!("{}", i + 1), FontId::proportional(28.0), Color32::from_gray(70));
                    }
                    Img::Failed(e) => {
                        let e = e.to_string();
                        let r = ui.interact(rect, ui.scope_id().with(("retry", i)), Sense::click());
                        ui.painter().text(rect.center(), Align2::CENTER_CENTER, format!("Page {} failed – click to retry\n{}", i + 1, e.chars().take(120).collect::<String>()), FontId::proportional(14.0), Color32::from_rgb(220, 120, 120));
                        if r.clicked() {
                            self.images.retry(&key);
                        }
                    }
                }
            }
            // Preload ahead (and one behind)
            for i in (last..(last + preload).min(n)).chain(first.checked_sub(1)) {
                let key = st.page_key(i);
                let src = st.page_src(i);
                self.images.prefetch(&key, || src, want_w);
            }
            // End of chapter
            let end = Rect::from_min_size(pos2(origin.x, origin.y + total), vec2(avail.x, end_h));
            if ui.is_rect_visible(end) {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(end.shrink(30.0)).layout(egui::Layout::top_down(egui::Align::Center)));
                if let Some(next) = st.sibling(true) {
                    child.label(RichText::new("End of chapter").color(Color32::GRAY));
                    if child.button(RichText::new(format!("Next: {}  ▶", st.chapters()[next].label())).size(16.0)).clicked() {
                        want_next = true;
                    }
                } else {
                    child.label(RichText::new("You're all caught up").color(Color32::GRAY));
                }
                if vp.max.y >= total + end_h - 2.0 {
                    st.page = n - 1;
                }
            }
        });
        st.scroll_y = out.state.clamped_offset().y;
        if want_next {
            self.next_chapter(st);
        }
    }

    fn paged(&mut self, ui: &mut Ui, st: &mut ReaderState, typing: bool) {
        let rtl = st.mode == ReaderMode::PagedRtl;
        let n = st.page_count();
        if let Some(p) = st.jump_page.take() {
            st.page = p.min(n - 1);
        }
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click());

        let (mut fwd, mut back) = (false, false);
        if !typing {
            ui.input(|i| {
                let right = i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::D);
                let left = i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::A);
                let (r_fwd, l_fwd) = if rtl { (false, true) } else { (true, false) };
                fwd |= (right && r_fwd) || (left && l_fwd) || i.key_pressed(Key::Space) || i.key_pressed(Key::PageDown) || i.key_pressed(Key::ArrowDown);
                back |= (right && !r_fwd) || (left && !l_fwd) || i.key_pressed(Key::PageUp) || i.key_pressed(Key::ArrowUp);
            });
        }
        if resp.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            st.wheel_acc += dy;
            if st.wheel_acc < -60.0 {
                fwd = true;
                st.wheel_acc = 0.0;
            } else if st.wheel_acc > 60.0 {
                back = true;
                st.wheel_acc = 0.0;
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let fx = (p.x - rect.left()) / rect.width();
                if fx < 0.33 {
                    if rtl { fwd = true } else { back = true }
                } else if fx > 0.67 {
                    if rtl { back = true } else { fwd = true }
                } else {
                    st.show_bar = !st.show_bar;
                }
            }
        }
        if fwd {
            if st.page + 1 < n {
                st.page += 1;
            } else {
                self.next_chapter(st);
                return;
            }
        }
        if back && st.page > 0 {
            st.page -= 1;
        }

        let ppp = ui.ctx().pixels_per_point();
        let i = st.page;
        let key = st.page_key(i);
        let src = st.page_src(i);
        // Decode wide enough for the fitted size
        let want_w = match st.aspects.get(i).copied().flatten() {
            Some(a) => (rect.width().min(rect.height() / a) * ppp).ceil() as u32,
            None => (rect.width() * ppp) as u32,
        };
        match self.images.get(&key, || src, want_w) {
            Img::Ready { tiles, size } => {
                let a = size[1] as f32 / size[0].max(1) as f32;
                st.aspects[i] = Some(a);
                let (mut w, mut h) = (rect.width(), rect.width() * a);
                if h > rect.height() {
                    h = rect.height();
                    w = h / a;
                }
                let r = Rect::from_center_size(rect.center(), vec2(w, h));
                paint_tiles(ui, r, tiles, size);
            }
            Img::Loading => {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Loading…", FontId::proportional(20.0), Color32::from_gray(90));
            }
            Img::Failed(e) => {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, format!("Failed: {e}\n(press R to retry)"), FontId::proportional(14.0), Color32::from_rgb(220, 120, 120));
                if ui.input(|i| i.key_pressed(Key::R)) {
                    self.images.retry(&key);
                }
            }
        }
        let preload = self.store.settings.preload_pages;
        let w = (rect.width() * ppp) as u32;
        for j in (i + 1..(i + 1 + preload).min(n)).chain(i.checked_sub(1)) {
            let key = st.page_key(j);
            let src = st.page_src(j);
            self.images.prefetch(&key, || src, w);
        }
        // Page indicator
        ui.painter().text(
            rect.center_bottom() - vec2(0.0, 10.0),
            Align2::CENTER_BOTTOM,
            format!("{} / {}", i + 1, n),
            FontId::proportional(12.0),
            Color32::from_white_alpha(120),
        );
    }
}
