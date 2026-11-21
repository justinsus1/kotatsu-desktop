use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    pub title: String,
    pub locale: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub broken: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tag {
    pub key: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Chapter {
    pub id: i64,
    pub title: Option<String>,
    #[serde(default)]
    pub number: f32,
    #[serde(default)]
    pub volume: i32,
    pub url: String,
    pub scanlator: Option<String>,
    #[serde(default)]
    pub date: i64,
    pub branch: Option<String>,
}

impl Chapter {
    pub fn label(&self) -> String {
        if let Some(t) = self.title.as_deref().filter(|t| !t.is_empty()) {
            return t.to_string();
        }
        let mut s = String::new();
        if self.volume > 0 {
            s.push_str(&format!("Vol {} ", self.volume));
        }
        if self.number > 0.0 {
            let n = self.number.to_string();
            s.push_str("Chapter ");
            s.push_str(n.strip_suffix(".0").unwrap_or(&n));
        } else {
            s.push_str("Unnamed");
        }
        s
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manga {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub alt: Vec<String>,
    pub url: String,
    #[serde(default, rename = "publicUrl")]
    pub public_url: String,
    #[serde(default)]
    pub rating: f32,
    #[serde(default)]
    pub adult: bool,
    pub cover: Option<String>,
    #[serde(rename = "largeCover")]
    pub large_cover: Option<String>,
    #[serde(default)]
    pub tags: Vec<Tag>,
    pub state: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    pub description: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapters: Option<Vec<Chapter>>,
}

impl Manga {
    /// Copy without the (potentially huge) chapter list, for storage.
    pub fn slim(&self) -> Manga {
        Manga { chapters: None, description: None, ..self.clone() }
    }
    pub fn key(&self) -> String {
        format!("{}:{}", self.source, self.id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub id: i64,
    pub url: String,
    pub preview: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SourceInfo {
    #[serde(default)]
    pub orders: Vec<String>,
    #[serde(default)]
    pub search: bool,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub domains: Vec<String>,
}

pub fn order_label(o: &str) -> &'static str {
    match o {
        "UPDATED" => "Updated",
        "UPDATED_ASC" => "Updated (oldest)",
        "POPULARITY" => "Popular",
        "POPULARITY_ASC" => "Popular (least)",
        "RATING" => "Rating",
        "RATING_ASC" => "Rating (lowest)",
        "NEWEST" => "Newest",
        "NEWEST_ASC" => "Oldest",
        "ALPHABETICAL" => "A-Z",
        "ALPHABETICAL_DESC" => "Z-A",
        "ADDED" => "Added",
        "ADDED_ASC" => "Added (oldest)",
        "RELEVANCE" => "Relevance",
        _ => "Other",
    }
}
