//! Minimal, fast EPUB 2/3 reader: container -> OPF (metadata, manifest, spine)
//! -> TOC (nav.xhtml or toc.ncx), and XHTML chapters converted to a small block
//! model that the UI lays out itself.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BookMeta {
    pub path: PathBuf,
    pub title: String,
    pub author: String,
    /// Zip entry of the cover image
    pub cover: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TocEntry {
    pub title: String,
    pub spine: usize,
    pub depth: usize,
}

#[derive(Clone, Debug)]
pub struct Book {
    pub meta: BookMeta,
    /// Zip entries of the reading order
    pub spine: Vec<String>,
    pub toc: Vec<TocEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub link: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Para,
    Heading(u8),
    Quote,
    ListItem,
    Pre,
}

#[derive(Clone, Debug)]
pub enum Block {
    Text { kind: Kind, spans: Vec<Span> },
    Image { entry: String },
    Rule,
}

type Zip = zip::ZipArchive<BufReader<File>>;

fn open(path: &Path) -> Result<Zip, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    zip::ZipArchive::new(BufReader::new(f)).map_err(|e| e.to_string())
}

fn read_entry(zip: &mut Zip, name: &str) -> Result<Vec<u8>, String> {
    let mut f = zip.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut buf = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

pub fn read_file_entry(path: &Path, entry: &str) -> Result<Vec<u8>, String> {
    read_entry(&mut open(path)?, entry)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve `href` relative to the zip entry `base` (a file path).
pub fn resolve(base: &str, href: &str) -> String {
    let href = percent_decode(href.split('#').next().unwrap_or(""));
    let mut parts: Vec<&str> = if href.starts_with('/') { vec![] } else { base.split('/').collect() };
    if !href.starts_with('/') {
        parts.pop();
    }
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes().flatten().find(|a| {
        let k: &str = a.key.as_ref();
        k == name || k.rsplit(':').next() == Some(name)
    }).map(|a| {
        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_else(|_| a.value.to_string())
    })
}

fn local(e: &str) -> String {
    e.rsplit(':').next().unwrap_or(e).to_ascii_lowercase()
}

fn reader(xml: &str) -> Reader<&[u8]> {
    let mut r = Reader::from_str(xml);
    let c = r.config_mut();
    c.check_end_names = false;
    c.expand_empty_elements = false;
    r
}

fn entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "shy" => "",
        "thinsp" | "ensp" | "emsp" => " ",
        "laquo" => "«",
        "raquo" => "»",
        "middot" => "·",
        "bull" => "•",
        "deg" => "°",
        "times" => "×",
        "eacute" => "é",
        "egrave" => "è",
        "agrave" => "à",
        "aacute" => "á",
        "ccedil" => "ç",
        "uuml" => "ü",
        "ouml" => "ö",
        "auml" => "ä",
        "szlig" => "ß",
        _ => return None,
    })
}

fn ref_text(r: &quick_xml::events::BytesRef) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    let name: String = r.to_string();
    entity(&name).map(String::from).unwrap_or_else(|| format!("&{name};"))
}

// ------------------------------------------------------------------ opening

fn rootfile(zip: &mut Zip) -> Result<String, String> {
    let xml = String::from_utf8_lossy(&read_entry(zip, "META-INF/container.xml")?).into_owned();
    let mut r = reader(&xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == "rootfile" => {
                if let Some(p) = attr(&e, "full-path") {
                    return Ok(p);
                }
            }
            Ok(Event::Eof) | Err(_) => return Err("no rootfile in container.xml".into()),
            _ => {}
        }
    }
}

struct Opf {
    title: String,
    author: String,
    cover: Option<String>,
    spine: Vec<String>,
    nav: Option<String>,
    ncx: Option<String>,
}

fn parse_opf(zip: &mut Zip, opf_path: &str, need_spine: bool) -> Result<Opf, String> {
    let xml = String::from_utf8_lossy(&read_entry(zip, opf_path)?).into_owned();
    let mut r = reader(&xml);
    let mut manifest: HashMap<String, (String, String, String)> = HashMap::new(); // id -> (href, type, props)
    let mut spine_ids = vec![];
    let mut toc_id = None;
    let (mut title, mut author) = (String::new(), String::new());
    let mut cover_id = None;
    let mut cur = String::new();
    let mut text = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                cur = local(e.name().as_ref());
                text.clear();
                if cur == "spine" {
                    toc_id = attr(&e, "toc");
                }
            }
            Ok(Event::Empty(e)) => match local(e.name().as_ref()).as_str() {
                "item" => {
                    if let (Some(id), Some(href)) = (attr(&e, "id"), attr(&e, "href")) {
                        manifest.insert(id, (resolve(opf_path, &href), attr(&e, "media-type").unwrap_or_default(), attr(&e, "properties").unwrap_or_default()));
                    }
                }
                "itemref" if need_spine => {
                    if let Some(id) = attr(&e, "idref") {
                        spine_ids.push(id);
                    }
                }
                "meta" if attr(&e, "name").as_deref() == Some("cover") => cover_id = attr(&e, "content"),
                _ => {}
            },
            Ok(Event::Text(t)) => text.push_str(&t.html_content()),
            Ok(Event::GeneralRef(g)) => text.push_str(&ref_text(&g)),
            Ok(Event::End(e)) => {
                let n = local(e.name().as_ref());
                if n == "title" && title.is_empty() {
                    title = text.trim().to_string();
                } else if n == "creator" && author.is_empty() {
                    author = text.trim().to_string();
                }
                cur.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("OPF: {e}")),
            _ => {}
        }
    }
    let _ = cur;
    let cover = cover_id
        .and_then(|id| manifest.get(&id).map(|m| m.0.clone()))
        .or_else(|| manifest.values().find(|m| m.2.split_whitespace().any(|p| p == "cover-image")).map(|m| m.0.clone()))
        .or_else(|| manifest.values().find(|m| m.1.starts_with("image/") && m.0.to_lowercase().contains("cover")).map(|m| m.0.clone()));
    let nav = manifest.values().find(|m| m.2.split_whitespace().any(|p| p == "nav")).map(|m| m.0.clone());
    let ncx = toc_id
        .and_then(|id| manifest.get(&id).map(|m| m.0.clone()))
        .or_else(|| manifest.values().find(|m| m.1 == "application/x-dtbncx+xml").map(|m| m.0.clone()));
    let spine = spine_ids.into_iter().filter_map(|id| manifest.get(&id).map(|m| m.0.clone())).collect();
    Ok(Opf { title, author, cover, spine, nav, ncx })
}

pub fn read_meta(path: &Path) -> Result<BookMeta, String> {
    let mut zip = open(path)?;
    let opf_path = rootfile(&mut zip)?;
    let opf = parse_opf(&mut zip, &opf_path, false)?;
    let fallback = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(BookMeta {
        path: path.to_path_buf(),
        title: if opf.title.is_empty() { fallback } else { opf.title },
        author: opf.author,
        cover: opf.cover,
    })
}

pub fn open_book(path: &Path) -> Result<Book, String> {
    let mut zip = open(path)?;
    let opf_path = rootfile(&mut zip)?;
    let opf = parse_opf(&mut zip, &opf_path, true)?;
    let spine = opf.spine;
    let index: HashMap<&str, usize> = spine.iter().enumerate().map(|(i, s)| (s.as_str(), i)).collect();
    let mut toc = vec![];
    if let Some(nav) = &opf.nav {
        if let Ok(d) = read_entry(&mut zip, nav) {
            toc = parse_nav(&String::from_utf8_lossy(&d), nav, &index);
        }
    }
    if toc.is_empty() {
        if let Some(ncx) = &opf.ncx {
            if let Ok(d) = read_entry(&mut zip, ncx) {
                toc = parse_ncx(&String::from_utf8_lossy(&d), ncx, &index);
            }
        }
    }
    let fallback = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Book {
        meta: BookMeta { path: path.to_path_buf(), title: if opf.title.is_empty() { fallback } else { opf.title }, author: opf.author, cover: opf.cover },
        spine,
        toc,
    })
}

fn parse_nav(xml: &str, base: &str, index: &HashMap<&str, usize>) -> Vec<TocEntry> {
    let mut r = reader(xml);
    let mut out = vec![];
    let mut in_toc = false;
    let mut nav_depth = 0;
    let mut depth = 0usize;
    let mut cur: Option<(String, usize)> = None;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => match local(e.name().as_ref()).as_str() {
                "nav" => {
                    nav_depth += 1;
                    if attr(&e, "type").is_some_and(|t| t.contains("toc")) {
                        in_toc = true;
                    }
                }
                "ol" | "ul" if in_toc => depth += 1,
                "a" | "span" if in_toc => {
                    let target = attr(&e, "href").and_then(|h| index.get(resolve(base, &h).as_str()).copied());
                    cur = Some((String::new(), target.unwrap_or(usize::MAX)));
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let Some(c) = cur.as_mut() {
                    c.0.push_str(&t.html_content());
                }
            }
            Ok(Event::GeneralRef(g)) => {
                if let Some(c) = cur.as_mut() {
                    c.0.push_str(&ref_text(&g));
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()).as_str() {
                "nav" => {
                    nav_depth -= 1;
                    if in_toc {
                        in_toc = false;
                        if !out.is_empty() {
                            break;
                        }
                    }
                    let _ = nav_depth;
                }
                "ol" | "ul" if in_toc => depth = depth.saturating_sub(1),
                "a" | "span" => {
                    if let Some((t, s)) = cur.take() {
                        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                        if !t.is_empty() && s != usize::MAX {
                            out.push(TocEntry { title: t, spine: s, depth: depth.saturating_sub(1) });
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn parse_ncx(xml: &str, base: &str, index: &HashMap<&str, usize>) -> Vec<TocEntry> {
    let mut r = reader(xml);
    let mut out = vec![];
    let mut depth = 0usize;
    let mut label = String::new();
    let mut in_text = false;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => match local(e.name().as_ref()).as_str() {
                "navpoint" => {
                    depth += 1;
                    label.clear();
                }
                "text" => in_text = true,
                _ => {}
            },
            Ok(Event::Empty(e)) if local(e.name().as_ref()) == "content" => {
                if let Some(s) = attr(&e, "src").and_then(|h| index.get(resolve(base, &h).as_str()).copied()) {
                    let t = label.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() {
                        out.push(TocEntry { title: t, spine: s, depth: depth.saturating_sub(1) });
                    }
                }
                label.clear();
            }
            Ok(Event::Text(t)) if in_text => label.push_str(&t.html_content()),
            Ok(Event::GeneralRef(g)) if in_text => label.push_str(&ref_text(&g)),
            Ok(Event::End(e)) => match local(e.name().as_ref()).as_str() {
                "navpoint" => depth = depth.saturating_sub(1),
                "text" => in_text = false,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

// ------------------------------------------------------------------ chapters

struct Builder {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    kind: Kind,
    bold: u32,
    italic: u32,
    mono: u32,
    link: u32,
    pre: u32,
    space: bool,
}

impl Builder {
    fn flush(&mut self) {
        let mut spans = std::mem::take(&mut self.spans);
        // trim paragraph edges
        while spans.first().is_some_and(|s| s.text.trim().is_empty() && s.text != "\n") {
            spans.remove(0);
        }
        while spans.last().is_some_and(|s| s.text.trim().is_empty()) {
            spans.pop();
        }
        if let Some(f) = spans.first_mut() {
            if self.kind != Kind::Pre {
                f.text = f.text.trim_start().to_string();
            }
        }
        if let Some(l) = spans.last_mut() {
            if self.kind != Kind::Pre {
                l.text = l.text.trim_end().to_string();
            }
        }
        if !spans.is_empty() {
            self.blocks.push(Block::Text { kind: self.kind, spans });
        }
        self.space = true;
    }

    fn text(&mut self, s: &str) {
        let style = |b: &Builder| Span { text: String::new(), bold: b.bold > 0, italic: b.italic > 0, mono: b.mono > 0, link: b.link > 0 };
        let mut t = String::with_capacity(s.len());
        if self.pre > 0 {
            t.push_str(s);
        } else {
            for c in s.chars() {
                if c.is_whitespace() && c != '\u{a0}' {
                    if !self.space {
                        t.push(' ');
                        self.space = true;
                    }
                } else {
                    t.push(c);
                    self.space = false;
                }
            }
        }
        if t.is_empty() {
            return;
        }
        let st = style(self);
        match self.spans.last_mut() {
            Some(l) if l.bold == st.bold && l.italic == st.italic && l.mono == st.mono && l.link == st.link => l.text.push_str(&t),
            _ => self.spans.push(Span { text: t, ..st }),
        }
    }
}

pub fn load_chapter(path: &Path, entry: &str) -> Result<Vec<Block>, String> {
    let data = read_file_entry(path, entry)?;
    let xml = String::from_utf8_lossy(&data);
    Ok(parse_chapter(&xml, entry))
}

pub fn parse_chapter(xml: &str, entry: &str) -> Vec<Block> {
    let mut r = reader(xml);
    let mut b = Builder { blocks: vec![], spans: vec![], kind: Kind::Para, bold: 0, italic: 0, mono: 0, link: 0, pre: 0, space: true };
    let mut skip = 0u32;
    let mut in_body = !xml.contains("<body");
    loop {
        let ev = match r.read_event() {
            Ok(e) => e,
            Err(_) => continue,
        };
        match ev {
            Event::Start(e) => {
                let n = local(e.name().as_ref());
                if n == "body" {
                    in_body = true;
                }
                if !in_body {
                    continue;
                }
                if matches!(n.as_str(), "head" | "style" | "script" | "title") {
                    skip += 1;
                }
                if skip > 0 {
                    continue;
                }
                start_tag(&mut b, &n, &e, entry);
            }
            Event::Empty(e) => {
                if skip > 0 || !in_body {
                    continue;
                }
                let n = local(e.name().as_ref());
                match n.as_str() {
                    "br" => {
                        b.spans.push(Span { text: "\n".into(), ..Default::default() });
                        b.space = true;
                    }
                    "hr" => {
                        b.flush();
                        b.blocks.push(Block::Rule);
                    }
                    "img" | "image" => image(&mut b, &e, entry),
                    _ => {}
                }
            }
            Event::End(e) => {
                let n = local(e.name().as_ref());
                if matches!(n.as_str(), "head" | "style" | "script" | "title") {
                    skip = skip.saturating_sub(1);
                    continue;
                }
                if skip > 0 || !in_body {
                    continue;
                }
                end_tag(&mut b, &n);
            }
            Event::Text(t) if skip == 0 && in_body => b.text(&t.html_content()),
            Event::CData(t) if skip == 0 && in_body => b.text(&t.to_string()),
            Event::GeneralRef(g) if skip == 0 && in_body => {
                let s = ref_text(&g);
                if s == "\u{a0}" {
                    b.text("\u{a0}");
                } else {
                    b.text(&s);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    b.flush();
    b.blocks
}

fn image(b: &mut Builder, e: &BytesStart, entry: &str) {
    if let Some(src) = attr(e, "src").or_else(|| attr(e, "href")) {
        if !src.starts_with("data:") && !src.starts_with("http") {
            b.flush();
            b.blocks.push(Block::Image { entry: resolve(entry, &src) });
        }
    }
}

fn start_tag(b: &mut Builder, n: &str, e: &BytesStart, entry: &str) {
    match n {
        "p" | "div" | "section" | "article" | "td" | "th" | "dt" | "dd" | "figcaption" | "tr" | "table" | "figure" | "header" | "footer" | "aside" => {
            b.flush();
            if b.kind == Kind::Heading(0) {
                b.kind = Kind::Para;
            }
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            b.flush();
            b.kind = Kind::Heading(n.as_bytes()[1] - b'0');
        }
        "blockquote" => {
            b.flush();
            b.kind = Kind::Quote;
        }
        "li" => {
            b.flush();
            b.kind = Kind::ListItem;
        }
        "pre" => {
            b.flush();
            b.kind = Kind::Pre;
            b.pre += 1;
            b.mono += 1;
        }
        "b" | "strong" => b.bold += 1,
        "i" | "em" | "cite" => b.italic += 1,
        "code" | "tt" | "kbd" | "samp" => b.mono += 1,
        "a" => b.link += 1,
        "img" | "image" => image(b, e, entry),
        "br" => {
            b.spans.push(Span { text: "\n".into(), ..Default::default() });
            b.space = true;
        }
        _ => {}
    }
}

fn end_tag(b: &mut Builder, n: &str) {
    match n {
        "p" | "div" | "section" | "article" | "td" | "th" | "dt" | "dd" | "figcaption" | "tr" | "table" | "figure" | "header" | "footer" | "aside" => b.flush(),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" => {
            b.flush();
            b.kind = Kind::Para;
        }
        "blockquote" => {
            b.flush();
            b.kind = Kind::Para;
        }
        "pre" => {
            b.flush();
            b.kind = Kind::Para;
            b.pre = b.pre.saturating_sub(1);
            b.mono = b.mono.saturating_sub(1);
        }
        "b" | "strong" => b.bold = b.bold.saturating_sub(1),
        "i" | "em" | "cite" => b.italic = b.italic.saturating_sub(1),
        "code" | "tt" | "kbd" | "samp" => b.mono = b.mono.saturating_sub(1),
        "a" => b.link = b.link.saturating_sub(1),
        _ => {}
    }
}

/// All .epub files under `dir`, recursively.
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("epub")) {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn opens_real_epub() {
        // KD_EPUB=/path/book.epub cargo test opens_real_epub -- --ignored --nocapture
        let p = std::env::var("KD_EPUB").expect("KD_EPUB");
        let meta = read_meta(Path::new(&p)).unwrap();
        let book = open_book(Path::new(&p)).unwrap();
        println!("title={:?} author={:?} cover={:?}", meta.title, meta.author, meta.cover);
        println!("spine={} toc={}", book.spine.len(), book.toc.len());
        let t = std::time::Instant::now();
        let mut blocks = 0;
        let mut images = 0;
        for e in &book.spine {
            let b = load_chapter(Path::new(&p), e).unwrap();
            images += b.iter().filter(|x| matches!(x, Block::Image { .. })).count();
            blocks += b.len();
        }
        println!("parsed all chapters: {blocks} blocks, {images} images in {:?}", t.elapsed());
        assert!(!book.spine.is_empty() && blocks > 50);
        if let Some(c) = &meta.cover {
            assert!(!read_file_entry(Path::new(&p), c).unwrap().is_empty());
        }
    }

    #[test]
    fn resolves_paths() {
        assert_eq!(resolve("OEBPS/content.opf", "text/ch1.xhtml"), "OEBPS/text/ch1.xhtml");
        assert_eq!(resolve("OEBPS/text/ch1.xhtml", "../images/a%20b.jpg#x"), "OEBPS/images/a b.jpg");
        assert_eq!(resolve("content.opf", "ch1.xhtml"), "ch1.xhtml");
    }

    #[test]
    fn parses_chapter_blocks() {
        let x = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>T</title><style>p{}</style></head>
        <body><h1>Chapter  One</h1><p>Hello <b>bold</b> and <i>it&#8217;s</i>&nbsp;fine.</p>
        <p><img src="../img/a.png"/></p><hr/><ul><li>one</li><li>two</li></ul></body></html>"#;
        let blocks = parse_chapter(x, "OEBPS/text/c.xhtml");
        let kinds: Vec<String> = blocks
            .iter()
            .map(|b| match b {
                Block::Text { kind, spans } => format!("{kind:?}:{}", spans.iter().map(|s| s.text.as_str()).collect::<String>()),
                Block::Image { entry } => format!("img:{entry}"),
                Block::Rule => "hr".into(),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "Heading(1):Chapter One",
                "Para:Hello bold and it’s\u{a0}fine.",
                "img:OEBPS/img/a.png",
                "hr",
                "ListItem:one",
                "ListItem:two"
            ]
        );
        if let Block::Text { spans, .. } = &blocks[1] {
            assert!(spans.iter().any(|s| s.bold && s.text == "bold"));
            assert!(spans.iter().any(|s| s.italic));
        }
    }
}
