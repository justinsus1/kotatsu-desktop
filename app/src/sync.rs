//! Phone sync through a Kotatsu sync server (the same account the Android app
//! uses). Favourites and history are exchanged; the network work runs on a
//! worker thread and comes back as a list of changes for the UI thread to apply.
//!
//! The server keeps whatever row reaches it last, so only rows that changed on
//! this side are ever sent.

use crate::bridge::Bridge;
use crate::model::{Manga, Tag};
use crate::store::{Favourite, HistoryEntry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

// ------------------------------------------------------------------ wire format

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TagDto {
    tag_id: i64,
    title: String,
    key: String,
    source: String,
    #[serde(default)]
    pinned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MangaDto {
    manga_id: i64,
    title: String,
    alt_title: Option<String>,
    url: String,
    #[serde(default)]
    public_url: String,
    #[serde(default)]
    rating: f32,
    content_rating: Option<String>,
    #[serde(default)]
    cover_url: String,
    large_cover_url: Option<String>,
    #[serde(default)]
    tags: Vec<TagDto>,
    state: Option<String>,
    author: Option<String>,
    source: String,
    #[serde(default)]
    nsfw: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CategoryDto {
    category_id: i32,
    created_at: i64,
    sort_key: i32,
    title: String,
    order: String,
    track: bool,
    show_in_lib: bool,
    deleted_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FavouriteDto {
    manga_id: i64,
    manga: Value,
    category_id: i32,
    sort_key: i32,
    pinned: bool,
    created_at: i64,
    deleted_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HistoryDto {
    manga_id: i64,
    manga: Value,
    created_at: i64,
    updated_at: i64,
    chapter_id: i64,
    page: i32,
    #[serde(default)]
    scroll: f32,
    #[serde(default)]
    percent: f32,
    #[serde(default = "minus_one")]
    chapters: i32,
    deleted_at: i64,
}

fn minus_one() -> i32 {
    -1
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history: Option<Vec<HistoryDto>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    categories: Option<Vec<CategoryDto>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    favourites: Option<Vec<FavouriteDto>>,
    #[serde(default)]
    timestamp: i64,
}

// ------------------------------------------------------------------ conversions

/// Kotlin's `String.longHashCode()` from the parsers library (UTF-16 code units).
fn long_hash(s: &str) -> i64 {
    let mut h: i64 = 1_125_899_906_842_597;
    for c in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(c as i64);
    }
    h
}

fn to_dto(m: &Manga) -> Value {
    let dto = MangaDto {
        manga_id: m.id,
        title: m.title.clone(),
        alt_title: m.alt.first().cloned(),
        url: m.url.clone(),
        public_url: m.public_url.clone(),
        rating: if m.rating > 0.0 { m.rating } else { -1.0 },
        content_rating: m.adult.then(|| "ADULT".to_string()),
        cover_url: m.cover.clone().unwrap_or_default(),
        large_cover_url: m.large_cover.clone(),
        tags: m
            .tags
            .iter()
            .map(|t| TagDto { tag_id: long_hash(&format!("{}_{}", t.key, m.source)), title: t.title.clone(), key: t.key.clone(), source: m.source.clone(), pinned: false })
            .collect(),
        state: m.state.clone(),
        author: m.authors.first().cloned(),
        source: m.source.clone(),
        nsfw: m.adult,
    };
    serde_json::to_value(dto).unwrap()
}

fn from_dto(v: &Value) -> Option<Manga> {
    let d: MangaDto = serde_json::from_value(v.clone()).ok()?;
    Some(Manga {
        id: d.manga_id,
        title: d.title,
        alt: d.alt_title.into_iter().filter(|s| !s.is_empty()).collect(),
        url: d.url,
        public_url: d.public_url,
        rating: d.rating.max(0.0),
        adult: d.nsfw || d.content_rating.as_deref() == Some("ADULT"),
        cover: Some(d.cover_url).filter(|s| !s.is_empty()),
        large_cover: d.large_cover_url,
        tags: d.tags.into_iter().map(|t| Tag { key: t.key, title: t.title }).collect(),
        state: d.state,
        authors: d.author.into_iter().collect(),
        description: None,
        source: d.source,
        chapters: None,
    })
}

// ------------------------------------------------------------------ snapshot / outcome

pub struct Snapshot {
    pub url: String,
    pub token: String,
    pub favourites: Vec<Favourite>,
    pub history: Vec<HistoryEntry>,
    pub fav_removed: HashMap<String, i64>,
    pub hist_removed: HashMap<String, i64>,
    pub remote_manga: HashMap<i64, Value>,
}

pub enum Op {
    AddFav(Manga, i64),
    RemoveFav(String),
    SetHistory(HistoryEntry),
    RemoveHistory(String),
}

#[derive(Default)]
pub struct Outcome {
    pub ops: Vec<Op>,
    pub remote_manga: HashMap<i64, Value>,
    pub sent: usize,
    pub error: Option<String>,
    pub unauthorized: bool,
}

// ------------------------------------------------------------------ http

struct Api<'a> {
    bridge: &'a Bridge,
    url: String,
    token: String,
}

impl Api<'_> {
    fn request(&self, method: &str, path: &str, body: Option<String>) -> Result<(u16, String), String> {
        let mut p = json!({
            "method": method,
            "url": format!("{}{}", self.url, path),
            "headers": {"Authorization": format!("Bearer {}", self.token), "Content-Type": "application/json"},
        });
        if let Some(b) = body {
            p["body"] = json!(b);
        }
        let r = self.bridge.call_blocking("http", p)?;
        let code = r["code"].as_u64().unwrap_or(0) as u16;
        let body = r["body"].as_str().unwrap_or("").to_string();
        Ok((code, body))
    }

    fn get(&self, path: &str) -> Result<SyncDto, String> {
        let (code, body) = self.request("GET", path, None)?;
        check(code, &body)?;
        serde_json::from_str(&body).map_err(|e| format!("bad server response: {e}"))
    }

    fn post(&self, path: &str, dto: &SyncDto) -> Result<(), String> {
        let (code, body) = self.request("POST", path, Some(serde_json::to_string(dto).unwrap()))?;
        check(code, &body)
    }
}

fn check(code: u16, body: &str) -> Result<(), String> {
    match code {
        200..=299 => Ok(()),
        401 | 403 => Err("UNAUTHORIZED".into()),
        _ => Err(format!("server error {code}: {}", body.trim_matches('"').chars().take(120).collect::<String>())),
    }
}

pub fn normalize_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.starts_with("http://") || u.starts_with("https://") {
        u.to_string()
    } else {
        format!("https://{u}")
    }
}

/// Log in (or register, if the server allows it) and return a token.
pub fn login(bridge: &Bridge, url: &str, email: &str, password: &str) -> Result<String, String> {
    let r = bridge.call_blocking(
        "http",
        json!({
            "method": "POST",
            "url": format!("{}/auth", normalize_url(url)),
            "headers": {"Content-Type": "application/json"},
            "body": json!({"email": email, "password": password}).to_string(),
        }),
    )?;
    let code = r["code"].as_u64().unwrap_or(0);
    let body = r["body"].as_str().unwrap_or("");
    if (200..300).contains(&code) {
        serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v["token"].as_str().map(String::from))
            .ok_or_else(|| "server sent no token".into())
    } else {
        Err(body.trim_matches('"').chars().take(160).collect::<String>().if_empty(format!("login failed ({code})")))
    }
}

trait IfEmpty {
    fn if_empty(self, alt: String) -> String;
}
impl IfEmpty for String {
    fn if_empty(self, alt: String) -> String {
        if self.trim().is_empty() { alt } else { self }
    }
}

// ------------------------------------------------------------------ the sync

pub fn run(bridge: &Bridge, snap: Snapshot) -> Outcome {
    let mut out = Outcome::default();
    let api = Api { bridge, url: normalize_url(&snap.url), token: snap.token.clone() };
    let res = sync_favourites(&api, &snap, &mut out).and_then(|_| sync_history(&api, &snap, &mut out));
    if let Err(e) = res {
        out.unauthorized = e == "UNAUTHORIZED";
        out.error = Some(if out.unauthorized { "Sync login expired – log in again".into() } else { e });
    }
    out
}

fn now_ms() -> i64 {
    crate::store::now() * 1000
}

fn manga_value(snap: &Snapshot, out: &Outcome, m: &Manga) -> Value {
    out.remote_manga.get(&m.id).or_else(|| snap.remote_manga.get(&m.id)).cloned().unwrap_or_else(|| to_dto(m))
}

fn sync_favourites(api: &Api, snap: &Snapshot, out: &mut Outcome) -> Result<(), String> {
    let remote = api.get("/resource/favourites")?;
    let cats = remote.categories.unwrap_or_default();
    let favs = remote.favourites.unwrap_or_default();

    let mut active_cats: Vec<&CategoryDto> = cats.iter().filter(|c| c.deleted_at == 0).collect();
    active_cats.sort_by_key(|c| c.sort_key);
    let active_ids: HashSet<i32> = active_cats.iter().map(|c| c.category_id).collect();
    let mut new_cat = None;
    let default_cat = match active_cats.first() {
        Some(c) => c.category_id,
        None => {
            let id = cats.iter().map(|c| c.category_id).max().unwrap_or(0) + 1;
            new_cat = Some(CategoryDto {
                category_id: id,
                created_at: now_ms(),
                sort_key: 0,
                title: "Favourites".into(),
                order: "NEWEST".into(),
                track: true,
                show_in_lib: true,
                deleted_at: 0,
            });
            id
        }
    };

    let mut by_manga: HashMap<i64, Vec<&FavouriteDto>> = HashMap::new();
    for f in &favs {
        out.remote_manga.insert(f.manga_id, f.manga.clone());
        by_manga.entry(f.manga_id).or_default().push(f);
    }
    let is_active = |f: &FavouriteDto| f.deleted_at == 0 && active_ids.contains(&f.category_id);
    let local: HashMap<i64, &Favourite> = snap.favourites.iter().map(|f| (f.manga.id, f)).collect();

    let mut send = Vec::new();
    for (id, rows) in &by_manga {
        let active: Vec<&FavouriteDto> = rows.iter().copied().filter(|f| is_active(f)).collect();
        if active.is_empty() || local.contains_key(id) {
            continue;
        }
        let Some(m) = from_dto(&active[0].manga) else { continue };
        let added = active.iter().map(|r| r.created_at).max().unwrap_or(0);
        match snap.fav_removed.get(&m.key()) {
            // Removed here after it was added there: remove it there too
            Some(&t) if t > added => {
                for r in active {
                    send.push(FavouriteDto { deleted_at: t, ..r.clone() });
                }
            }
            _ => out.ops.push(Op::AddFav(m, added)),
        }
    }
    for f in &snap.favourites {
        let rows = by_manga.get(&f.manga.id);
        if rows.is_some_and(|r| r.iter().any(|f| is_active(f))) {
            continue;
        }
        let last_delete = rows.map(|r| r.iter().map(|x| x.deleted_at).max().unwrap_or(0)).unwrap_or(0);
        if last_delete > f.added * 1000 {
            out.ops.push(Op::RemoveFav(f.manga.key()));
        } else {
            send.push(FavouriteDto {
                manga_id: f.manga.id,
                manga: manga_value(snap, out, &f.manga),
                category_id: default_cat,
                sort_key: 0,
                pinned: false,
                created_at: f.added * 1000,
                deleted_at: 0,
            });
        }
    }
    if !send.is_empty() {
        out.sent += send.len();
        let dto = SyncDto {
            history: None,
            categories: Some(new_cat.into_iter().collect()),
            favourites: Some(send),
            timestamp: now_ms(),
        };
        api.post("/resource/favourites", &dto)?;
    }
    Ok(())
}

fn sync_history(api: &Api, snap: &Snapshot, out: &mut Outcome) -> Result<(), String> {
    let remote = api.get("/resource/history")?.history.unwrap_or_default();
    let local: HashMap<i64, &HistoryEntry> = snap.history.iter().map(|h| (h.manga.id, h)).collect();
    let mut seen = HashSet::new();
    let mut send = Vec::new();

    for r in &remote {
        seen.insert(r.manga_id);
        out.remote_manga.insert(r.manga_id, r.manga.clone());
        let l = local.get(&r.manga_id);
        if r.deleted_at != 0 {
            if let Some(l) = l {
                if l.at * 1000 < r.deleted_at {
                    out.ops.push(Op::RemoveHistory(l.manga.key()));
                }
            }
            continue;
        }
        match l {
            None => {
                let Some(m) = from_dto(&r.manga) else { continue };
                match snap.hist_removed.get(&m.key()) {
                    Some(&t) if t > r.updated_at => send.push(HistoryDto { deleted_at: t, ..r.clone() }),
                    _ => out.ops.push(Op::SetHistory(entry_from(r, m, None))),
                }
            }
            Some(l) => {
                let local_ms = l.at * 1000;
                if r.updated_at > local_ms + 999 {
                    if r.chapter_id != l.chapter_id || r.page as usize != l.page {
                        out.ops.push(Op::SetHistory(entry_from(r, l.manga.clone(), Some(l))));
                    }
                } else if local_ms > r.updated_at + 999 {
                    send.push(history_row(snap, out, l, r.created_at));
                }
            }
        }
    }
    for l in &snap.history {
        if !seen.contains(&l.manga.id) {
            send.push(history_row(snap, out, l, l.at * 1000));
        }
    }
    if !send.is_empty() {
        out.sent += send.len();
        api.post("/resource/history", &SyncDto { history: Some(send), categories: None, favourites: None, timestamp: now_ms() })?;
    }
    Ok(())
}

fn entry_from(r: &HistoryDto, manga: Manga, local: Option<&HistoryEntry>) -> HistoryEntry {
    let label = local.filter(|l| l.chapter_id == r.chapter_id).map(|l| l.chapter_label.clone()).unwrap_or_default();
    HistoryEntry {
        manga,
        chapter_id: r.chapter_id,
        chapter_label: label,
        page: r.page.max(0) as usize,
        at: r.updated_at / 1000,
        percent: r.percent.max(0.0),
        chapters: r.chapters,
    }
}

fn history_row(snap: &Snapshot, out: &Outcome, l: &HistoryEntry, created_at: i64) -> HistoryDto {
    HistoryDto {
        manga_id: l.manga.id,
        manga: manga_value(snap, out, &l.manga),
        created_at,
        updated_at: l.at * 1000,
        chapter_id: l.chapter_id,
        page: l.page as i32,
        scroll: 0.0,
        percent: l.percent,
        chapters: l.chapters,
        deleted_at: 0,
    }
}

#[cfg(test)]
mod mock_test {
    //! End-to-end against tests/mock_sync_server.py:
    //!   python3 tests/mock_sync_server.py 8765 &
    //!   KD_SYNC_URL=http://127.0.0.1:8765 KOTATSU_BRIDGE=../bridge/build/native/kotatsu-bridge cargo test -- --ignored
    use super::*;
    use crate::store::{Favourite, HistoryEntry};

    fn manga(id: i64, title: &str) -> Manga {
        Manga { id, title: title.into(), url: format!("/m/{id}"), source: "MANGADEX".into(), cover: Some("https://x/c.jpg".into()), ..Default::default() }
    }

    #[test]
    #[ignore]
    fn sync_against_mock() {
        let url = std::env::var("KD_SYNC_URL").expect("KD_SYNC_URL");
        let bridge = Bridge::start(std::env::temp_dir().join("kd-sync-test"), || {});
        let t = now_ms();
        let row = |m: &Manga, cat: i32, created: i64, deleted: i64| json!({"manga_id": m.id, "manga": to_dto(m), "category_id": cat, "sort_key": 0, "pinned": false, "created_at": created, "deleted_at": deleted});
        let (a, b, c, d, e) = (manga(1, "A phone-only"), manga(2, "B deleted on phone"), manga(3, "C desktop-only"), manga(4, "D removed on desktop"), manga(5, "E history"));
        let seed = json!({
            "cats": [{"category_id": 7, "created_at": 1, "sort_key": 0, "title": "Reading", "order": "NEWEST", "track": true, "show_in_lib": true, "deleted_at": 0}],
            "favs": [row(&a, 7, t - 50_000, 0), row(&b, 7, t - 90_000, t - 10_000), row(&d, 7, t - 90_000, 0)],
            "hist": [
                {"manga_id": 1, "manga": to_dto(&a), "created_at": t - 99_000, "updated_at": t - 5_000, "chapter_id": 11, "page": 4, "scroll": 0.0, "percent": 0.5, "chapters": 10, "deleted_at": 0},
                {"manga_id": 5, "manga": to_dto(&e), "created_at": t - 99_000, "updated_at": t - 80_000, "chapter_id": 51, "page": 1, "scroll": 0.0, "percent": 0.1, "chapters": 10, "deleted_at": 0},
            ],
        });
        bridge.call_blocking("http", json!({"method": "POST", "url": format!("{url}/seed"), "body": seed.to_string(), "headers": {"Content-Type": "application/json"}})).unwrap();

        assert!(login(&bridge, &url, "me@x", "bad").is_err());
        let token = login(&bridge, &url, "me@x", "pw").unwrap();

        let fav = |m: &Manga, added_s: i64| Favourite { manga: m.clone(), added: added_s, known_chapters: 0, new_chapters: 0 };
        let hist = |m: &Manga, ch: i64, page: usize, at_s: i64| HistoryEntry { manga: m.clone(), chapter_id: ch, chapter_label: "x".into(), page, at: at_s, percent: 0.3, chapters: 10 };
        let snap = Snapshot {
            url: url.clone(),
            token,
            favourites: vec![fav(&b, (t - 90_000) / 1000), fav(&c, (t - 20_000) / 1000)],
            history: vec![hist(&a, 10, 2, (t - 60_000) / 1000), hist(&e, 52, 7, (t - 2_000) / 1000)],
            fav_removed: [(d.key(), t - 1_000)].into_iter().collect(),
            hist_removed: Default::default(),
            remote_manga: Default::default(),
        };
        let out = run(&bridge, snap);
        assert!(out.error.is_none(), "{:?}", out.error);

        let mut adds = vec![];
        let mut removes = vec![];
        let mut hist_set = vec![];
        for op in &out.ops {
            match op {
                Op::AddFav(m, _) => adds.push(m.id),
                Op::RemoveFav(k) => removes.push(k.clone()),
                Op::SetHistory(h) => hist_set.push((h.manga.id, h.chapter_id, h.page)),
                Op::RemoveHistory(_) => {}
            }
        }
        assert_eq!(adds, vec![1], "A should come from the phone");
        assert_eq!(removes, vec![b.key()], "B was deleted on the phone");
        assert_eq!(hist_set, vec![(1, 11, 4)], "phone progress on A is newer");

        let dump = bridge.call_blocking("http", json!({"method": "GET", "url": format!("{url}/dump")})).unwrap();
        let dump: Value = serde_json::from_str(dump["body"].as_str().unwrap()).unwrap();
        let favs = dump["favs"].as_array().unwrap();
        let get = |id: i64| favs.iter().find(|f| f["manga_id"] == id).cloned().unwrap();
        assert_eq!(get(3)["deleted_at"], 0, "C pushed to the server");
        assert_eq!(get(3)["category_id"], 7, "into the phone's first category");
        assert!(get(4)["deleted_at"].as_i64().unwrap() > 0, "D removal pushed");
        assert!(get(2)["deleted_at"].as_i64().unwrap() > 0, "B stays deleted");
        let h5 = dump["hist"].as_array().unwrap().iter().find(|h| h["manga_id"] == 5).cloned().unwrap();
        assert_eq!((h5["chapter_id"].as_i64(), h5["page"].as_i64()), (Some(52), Some(7)), "desktop progress on E pushed");
        let h1 = dump["hist"].as_array().unwrap().iter().find(|h| h["manga_id"] == 1).cloned().unwrap();
        assert_eq!(h1["chapter_id"], 11, "phone progress on A not overwritten");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_hash_matches_kotlin() {
        // "abc".longHashCode() in Kotlin, with the parsers' seed
        let seed: i64 = 1_125_899_906_842_597;
        let expect = ((seed.wrapping_mul(31) + 97).wrapping_mul(31) + 98).wrapping_mul(31) + 99;
        assert_eq!(super::long_hash("abc"), expect);
    }
}
