//! OPDS 1.x catalogue support (Calibre content server, Kavita, Komga, COPS, …).

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub title: String,
    pub author: String,
    pub summary: String,
    /// A sub-catalogue to open
    pub nav: Option<String>,
    /// Downloadable EPUB
    pub epub: Option<String>,
    pub cover: Option<String>,
    pub id: String,
}

#[derive(Clone, Debug, Default)]
pub struct Feed {
    pub title: String,
    pub entries: Vec<Entry>,
    pub next: Option<String>,
    /// Either a URL template containing `{searchTerms}`, or an OpenSearch description URL
    pub search: Option<String>,
}

/// Resolve `href` against `base` (an absolute URL).
pub fn join(base: &str, href: &str) -> String {
    if href.contains("://") {
        return href.to_string();
    }
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    let origin_end = base[scheme_end..].find('/').map(|i| i + scheme_end).unwrap_or(base.len());
    let origin = &base[..origin_end];
    if href.starts_with("//") {
        return format!("{}{}", &base[..scheme_end.saturating_sub(2)], href);
    }
    if href.starts_with('/') {
        return format!("{origin}{href}");
    }
    let path = base[origin_end..].split(['?', '#']).next().unwrap_or("");
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let (href_path, query) = match href.find('?') {
        Some(i) => (&href[..i], &href[i..]),
        None => (href, ""),
    };
    for seg in href_path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let trailing = if href_path.ends_with('/') { "/" } else { "" };
    format!("{origin}/{}{trailing}{query}", parts.join("/"))
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes().flatten().find(|a| {
        let k: &str = a.key.as_ref();
        k == name
    }).map(|a| {
        a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|v| v.into_owned()).unwrap_or_else(|_| a.value.to_string())
    })
}

fn local(n: &str) -> String {
    n.rsplit(':').next().unwrap_or(n).to_ascii_lowercase()
}

fn is_epub(t: &str) -> bool {
    t.starts_with("application/epub+zip")
}

pub fn parse_feed(xml: &str, base: &str) -> Result<Feed, String> {
    let mut r = Reader::from_str(xml);
    r.config_mut().check_end_names = false;
    let mut feed = Feed::default();
    let mut cur: Option<Entry> = None;
    let mut path: Vec<String> = vec![];
    let mut text = String::new();
    let mut saw_feed = false;

    let mut on_link = |e: &BytesStart, cur: &mut Option<Entry>, feed: &mut Feed| {
        let rel = attr(e, "rel").unwrap_or_default();
        let typ = attr(e, "type").unwrap_or_default();
        let Some(href) = attr(e, "href") else { return };
        let url = if href.contains("{searchTerms}") { href.clone() } else { join(base, &href) };
        match cur {
            Some(en) => {
                if rel.contains("opds-spec.org/acquisition") || (rel.is_empty() && is_epub(&typ)) {
                    if is_epub(&typ) && en.epub.is_none() {
                        en.epub = Some(url);
                    }
                } else if rel == "http://opds-spec.org/image" || rel == "http://opds-spec.org/cover" {
                    en.cover = Some(url);
                } else if rel.contains("opds-spec.org/image/thumbnail") || rel.contains("opds-spec.org/thumbnail") {
                    if en.cover.is_none() {
                        en.cover = Some(url);
                    }
                } else if typ.contains("application/atom+xml") || typ.contains("opds-catalog") {
                    if en.nav.is_none() {
                        en.nav = Some(url);
                    }
                }
            }
            None => match rel.as_str() {
                "next" => feed.next = Some(url),
                "search" => {
                    // Prefer a direct template over an OpenSearch description
                    if feed.search.as_ref().is_none_or(|s| !s.contains("{searchTerms}")) {
                        feed.search = Some(url);
                    }
                }
                _ => {}
            },
        }
    };

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let n = local(e.name().as_ref());
                if n == "feed" {
                    saw_feed = true;
                }
                if n == "entry" {
                    cur = Some(Entry::default());
                }
                if n == "link" {
                    on_link(&e, &mut cur, &mut feed);
                }
                path.push(n);
                text.clear();
            }
            Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == "link" {
                    on_link(&e, &mut cur, &mut feed);
                }
            }
            Ok(Event::Text(t)) => text.push_str(&t.html_content()),
            Ok(Event::CData(t)) => text.push_str(&t.to_string()),
            Ok(Event::GeneralRef(g)) => {
                if let Ok(Some(c)) = g.resolve_char_ref() {
                    text.push(c);
                } else {
                    text.push_str(match &*g.to_string() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => " ",
                    });
                }
            }
            Ok(Event::End(e)) => {
                let n = local(e.name().as_ref());
                let parent = path.iter().rev().nth(1).cloned().unwrap_or_default();
                let t = text.trim().to_string();
                match (n.as_str(), cur.as_mut()) {
                    ("entry", Some(_)) => {
                        let en = cur.take().unwrap();
                        if en.nav.is_some() || en.epub.is_some() {
                            feed.entries.push(en);
                        }
                    }
                    ("title", Some(en)) if parent == "entry" => en.title = t,
                    ("title", None) if parent == "feed" => feed.title = t,
                    ("name", Some(en)) if parent == "author" && en.author.is_empty() => en.author = t,
                    ("summary" | "content", Some(en)) if en.summary.is_empty() => en.summary = strip_tags(&t),
                    ("id", Some(en)) => en.id = t,
                    _ => {}
                }
                path.pop();
                text.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("not a valid OPDS feed: {e}")),
            _ => {}
        }
    }
    if !saw_feed {
        return Err("not an OPDS catalogue (no Atom feed)".into());
    }
    Ok(feed)
}

/// Read the search template out of an OpenSearch description document.
pub fn parse_opensearch(xml: &str, base: &str) -> Option<String> {
    let mut r = Reader::from_str(xml);
    let mut best = None;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == "url" => {
                let typ = attr(&e, "type").unwrap_or_default();
                if let Some(t) = attr(&e, "template") {
                    let t = if t.contains("://") { t } else { join(base, &t) };
                    if typ.contains("atom") {
                        return Some(t);
                    }
                    best.get_or_insert(t);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    best
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn search_url(template: &str, q: &str) -> String {
    let enc: String = q
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    template
        .replace("{searchTerms}", &enc)
        .replace("{startPage?}", "1")
        .replace("{startIndex?}", "0")
        .replace("{count?}", "")
        .replace("{language?}", "")
}

#[cfg(test)]
mod live {
    //! Against a real calibre server:
    //!   KD_OPDS=http://127.0.0.1:8099/opds KD_OPDS_AUTH=user:pass KOTATSU_BRIDGE=… cargo test opds_live -- --ignored --nocapture
    use super::*;
    use serde_json::json;

    #[test]
    #[ignore]
    fn opds_live() {
        let root = std::env::var("KD_OPDS").unwrap();
        let bridge = crate::bridge::Bridge::start(std::env::temp_dir().join("kd-opds"), || {});
        if let Ok(a) = std::env::var("KD_OPDS_AUTH") {
            let host = root.split("://").nth(1).unwrap().split('/').next().unwrap().to_string();
            let enc = crate::books::test_b64(&a);
            bridge.call_blocking("setAuth", json!({"host": host, "header": format!("Basic {enc}")})).unwrap();
        }
        let get = |u: &str| {
            let r = bridge.call_blocking("http", json!({"method": "GET", "url": u})).unwrap();
            assert_eq!(r["code"], 200, "{u}");
            parse_feed(r["body"].as_str().unwrap(), u).unwrap()
        };
        let feed = get(&root);
        println!("root '{}': {} entries, search={:?}", feed.title, feed.entries.len(), feed.search);
        let by_title = feed.entries.iter().find(|e| e.title.contains("Title")).unwrap().nav.clone().unwrap();
        let books = get(&by_title);
        let b = books.entries.iter().find(|e| e.epub.is_some()).expect("a book");
        println!("book: {} / {} epub={:?} cover={:?}", b.title, b.author, b.epub, b.cover);
        let s = get(&search_url(&join(&root, feed.search.as_deref().unwrap()), "frankenstein"));
        assert!(s.entries.iter().any(|e| e.epub.is_some()), "search finds it");
        let dest = std::env::temp_dir().join("kd-opds/book.epub");
        bridge.call_blocking("download", json!({"urls": [b.epub.clone().unwrap()], "dest": dest})).unwrap();
        let meta = crate::epub::read_meta(&dest).unwrap();
        println!("downloaded + opened: {}", meta.title);
        let cover = std::env::temp_dir().join("kd-opds/cover");
        bridge.call_blocking("download", json!({"urls": [b.cover.clone().unwrap()], "dest": cover})).unwrap();
        assert!(std::fs::metadata(&cover).unwrap().len() > 1000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_urls() {
        assert_eq!(join("http://h:8080/opds", "/opds/navcatalog/4f"), "http://h:8080/opds/navcatalog/4f");
        assert_eq!(join("http://h/a/b/feed.xml", "c/d.epub"), "http://h/a/b/c/d.epub");
        assert_eq!(join("http://h/a/b/", "../x?y=1"), "http://h/a/x?y=1");
        assert_eq!(join("https://h/a", "https://o/x"), "https://o/x");
    }

    #[test]
    fn parses_feed() {
        let xml = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:opds="http://opds-spec.org/2010/catalog">
  <title>My Library</title>
  <link rel="next" href="/opds?offset=25" type="application/atom+xml"/>
  <link rel="search" type="application/atom+xml" href="/opds/search/{searchTerms}"/>
  <entry><title>By Author</title><id>a</id>
    <link type="application/atom+xml;profile=opds-catalog;kind=navigation" href="/opds/authors"/></entry>
  <entry><title>Frankenstein</title><id>b</id><author><name>Mary Shelley</name></author>
    <summary type="text">A &amp; B</summary>
    <link rel="http://opds-spec.org/acquisition" type="application/epub+zip" href="/get/epub/3"/>
    <link rel="http://opds-spec.org/image" type="image/jpeg" href="/get/cover/3"/></entry>
</feed>"#;
        let f = parse_feed(xml, "http://h:8080/opds").unwrap();
        assert_eq!(f.title, "My Library");
        assert_eq!(f.next.as_deref(), Some("http://h:8080/opds?offset=25"));
        assert_eq!(f.search.as_deref(), Some("/opds/search/{searchTerms}"));
        assert_eq!(f.entries.len(), 2);
        assert_eq!(f.entries[0].nav.as_deref(), Some("http://h:8080/opds/authors"));
        let b = &f.entries[1];
        assert_eq!((b.title.as_str(), b.author.as_str(), b.summary.as_str()), ("Frankenstein", "Mary Shelley", "A & B"));
        assert_eq!(b.epub.as_deref(), Some("http://h:8080/get/epub/3"));
        assert_eq!(b.cover.as_deref(), Some("http://h:8080/get/cover/3"));
        assert_eq!(search_url(&join("http://h:8080/opds", f.search.as_deref().unwrap()), "mary shelley"), "http://h:8080/opds/search/mary%20shelley");
    }
}
