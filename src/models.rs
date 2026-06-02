//! Domain models: item types, the DB row, the output DTO, and the
//! create/update payloads (including type-specific field folding into `extra`).

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{AppError, AppResult};

/// The three card types. Stored canonically in English.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemType {
    Game,
    Movie,
    Book,
}

impl ItemType {
    /// Canonical lowercase English string stored in the DB.
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemType::Game => "game",
            ItemType::Movie => "movie",
            ItemType::Book => "book",
        }
    }

    /// Parse from English or Chinese input. Returns `BadRequest` on unknown.
    pub fn parse(input: &str) -> AppResult<Self> {
        match input.trim() {
            "game" | "Game" | "GAME" | "游戏" => Ok(ItemType::Game),
            "movie" | "Movie" | "MOVIE" | "电影" => Ok(ItemType::Movie),
            "book" | "Book" | "BOOK" | "图书" => Ok(ItemType::Book),
            other => Err(AppError::BadRequest(format!(
                "unknown type '{other}' (expected game/movie/book or 游戏/电影/图书)"
            ))),
        }
    }

    /// Parse an optional filter value. `all`/empty/None -> None (no filter).
    pub fn parse_filter(input: Option<&str>) -> AppResult<Option<Self>> {
        match input.map(str::trim) {
            None | Some("") | Some("all") | Some("全部") => Ok(None),
            Some(other) => Ok(Some(Self::parse(other)?)),
        }
    }
}

/// Field names recognised as type-specific and folded into `extra`.
pub const EXTRA_FIELD_KEYS: &[&str] = &[
    // game
    "developer",
    "publisher",
    "platforms",
    // movie
    "director",
    "writers",
    "cast",
    "country",
    "language",
    "duration",
    "imdb",
    // book
    "author",
    "isbn",
    "pages",
    "price",
    "binding",
    "series",
];

/// A full row read from the `favorites` table.
#[derive(Debug, Clone)]
pub struct Favorite {
    pub id: i64,
    pub item_type: String,
    pub name: String,
    pub url: Option<String>,
    /// Raw JSON text for `aka` (array or string), as stored.
    pub aka: Option<String>,
    /// Raw JSON array text for `genres`, as stored.
    pub genres: Option<String>,
    pub release_date: Option<String>,
    pub rating: Option<f64>,
    pub summary: Option<String>,
    /// Raw JSON object text for `extra`.
    pub extra: String,
    pub has_image: bool,
    pub sort_date: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Output DTO. Never includes the raw image blob.
#[derive(Debug, Serialize)]
pub struct FavoriteDto {
    pub id: i64,
    #[serde(rename = "type")]
    pub item_type: String,
    pub name: String,
    pub url: Option<String>,
    pub aka: Value,
    pub genres: Value,
    pub release_date: Option<String>,
    pub rating: Option<f64>,
    pub summary: Option<String>,
    pub extra: Value,
    pub has_image: bool,
    pub image_url: Option<String>,
    pub sort_date: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&Favorite> for FavoriteDto {
    fn from(f: &Favorite) -> Self {
        FavoriteDto {
            id: f.id,
            item_type: f.item_type.clone(),
            name: f.name.clone(),
            url: f.url.clone(),
            aka: parse_json_or_null(f.aka.as_deref()),
            genres: parse_json_array(f.genres.as_deref()),
            release_date: f.release_date.clone(),
            rating: f.rating,
            summary: f.summary.clone(),
            extra: parse_json_object(Some(&f.extra)),
            has_image: f.has_image,
            image_url: if f.has_image {
                Some(format!("/api/favorites/{}/image", f.id))
            } else {
                None
            },
            sort_date: f.sort_date.clone(),
            created_at: f.created_at.clone(),
            updated_at: f.updated_at.clone(),
        }
    }
}

/// Parse a JSON text into a `Value`; returns `Null` if absent/invalid.
fn parse_json_or_null(text: Option<&str>) -> Value {
    match text {
        Some(t) if !t.trim().is_empty() => serde_json::from_str(t).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// Parse a JSON array text; returns an empty array if absent/invalid.
fn parse_json_array(text: Option<&str>) -> Value {
    match text {
        Some(t) if !t.trim().is_empty() => {
            serde_json::from_str(t).unwrap_or_else(|_| Value::Array(vec![]))
        }
        _ => Value::Array(vec![]),
    }
}

/// Parse a JSON object text; returns an empty object if absent/invalid.
fn parse_json_object(text: Option<&str>) -> Value {
    match text {
        Some(t) if !t.trim().is_empty() => {
            serde_json::from_str(t).unwrap_or_else(|_| Value::Object(Map::new()))
        }
        _ => Value::Object(Map::new()),
    }
}

/// Raw incoming JSON body for create/update. We accept it as a free-form
/// object so convenience top-level type fields can be folded into `extra`.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct RawPayload(pub Map<String, Value>);

/// Normalised values prepared for an INSERT.
#[derive(Debug)]
pub struct PreparedCreate {
    pub item_type: ItemType,
    pub name: String,
    pub url: Option<String>,
    pub aka: Option<String>,
    pub genres: Option<String>,
    pub release_date: Option<String>,
    pub rating: Option<f64>,
    pub summary: Option<String>,
    pub extra: String,
    pub sort_date: String,
    pub image_base64: Option<String>,
    pub image_url: Option<String>,
    pub now: String,
}

/// Normalised values prepared for a partial UPDATE. `None` = leave unchanged.
#[derive(Debug, Default)]
pub struct PreparedUpdate {
    pub item_type: Option<ItemType>,
    pub name: Option<String>,
    pub url: Option<Option<String>>,
    pub aka: Option<Option<String>>,
    pub genres: Option<Option<String>>,
    pub release_date: Option<Option<String>>,
    pub rating: Option<Option<f64>>,
    pub summary: Option<Option<String>>,
    pub extra: Option<String>,
    pub sort_date: Option<String>,
    pub image_base64: Option<String>,
    pub image_url: Option<String>,
}

/// Current UTC time as an ISO-8601 string.
pub fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// Today's date (UTC) as YYYY-MM-DD.
pub fn today_date() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

/// Take a string field from the body, trimming; error if present but empty
/// when `required` is set.
fn take_string(body: &Map<String, Value>, key: &str) -> Option<String> {
    match body.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

/// Take and validate the optional `url` field: must be an http(s) URL when
/// present (rejects `javascript:`/`data:` schemes — defence against stored XSS
/// when the value is later used as a link `href`).
fn take_url(body: &Map<String, Value>) -> AppResult<Option<String>> {
    match take_string(body, "url").map(|s| s.trim().to_string()) {
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => {
            if s.starts_with("http://") || s.starts_with("https://") {
                Ok(Some(s))
            } else {
                Err(AppError::BadRequest(
                    "field 'url' must be an http(s) URL".into(),
                ))
            }
        }
        None => Ok(None),
    }
}

/// Serialise the `aka` field: accept array or string, store as JSON.
fn normalise_aka(body: &Map<String, Value>) -> AppResult<Option<String>> {
    match body.get("aka") {
        None | Some(Value::Null) => Ok(None),
        Some(v @ Value::Array(_)) | Some(v @ Value::String(_)) => {
            Ok(Some(serde_json::to_string(v)?))
        }
        Some(_) => Err(AppError::BadRequest(
            "field 'aka' must be a string or array of strings".into(),
        )),
    }
}

/// Serialise the `genres` field: accept array (or string -> single-element).
fn normalise_genres(body: &Map<String, Value>) -> AppResult<Option<String>> {
    match body.get("genres") {
        None | Some(Value::Null) => Ok(None),
        Some(arr @ Value::Array(_)) => Ok(Some(serde_json::to_string(arr)?)),
        Some(Value::String(s)) => {
            let arr = Value::Array(vec![Value::String(s.clone())]);
            Ok(Some(serde_json::to_string(&arr)?))
        }
        Some(_) => Err(AppError::BadRequest(
            "field 'genres' must be an array of strings".into(),
        )),
    }
}

/// Build the `extra` JSON object by merging an explicit `extra` object with
/// any recognised top-level convenience fields. Top-level fields win only
/// when not already present in the explicit `extra` object.
fn fold_extra(body: &Map<String, Value>) -> AppResult<Map<String, Value>> {
    let mut extra = match body.get("extra") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(o)) => o.clone(),
        Some(_) => {
            return Err(AppError::BadRequest("field 'extra' must be an object".into()));
        }
    };

    for &key in EXTRA_FIELD_KEYS {
        if let Some(val) = body.get(key) {
            if !val.is_null() && !extra.contains_key(key) {
                extra.insert(key.to_string(), val.clone());
            }
        }
    }
    Ok(extra)
}

/// Parse an optional rating, validating the 0..=10 range.
fn parse_rating(body: &Map<String, Value>) -> AppResult<Option<f64>> {
    match body.get("rating") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let r = n
                .as_f64()
                .ok_or_else(|| AppError::BadRequest("field 'rating' must be a number".into()))?;
            if !(0.0..=10.0).contains(&r) {
                return Err(AppError::BadRequest(
                    "field 'rating' must be between 0 and 10".into(),
                ));
            }
            Ok(Some(r))
        }
        Some(_) => Err(AppError::BadRequest("field 'rating' must be a number".into())),
    }
}

/// At most one image source may be supplied.
fn extract_image_sources(
    body: &Map<String, Value>,
) -> AppResult<(Option<String>, Option<String>)> {
    let image_base64 = take_string(body, "image_base64").filter(|s| !s.trim().is_empty());
    let image_url = take_string(body, "image_url").filter(|s| !s.trim().is_empty());
    if image_base64.is_some() && image_url.is_some() {
        return Err(AppError::BadRequest(
            "provide at most one of 'image_base64' or 'image_url'".into(),
        ));
    }
    Ok((image_base64, image_url))
}

impl PreparedCreate {
    /// Validate and normalise a create payload.
    pub fn from_body(body: &Map<String, Value>) -> AppResult<Self> {
        let item_type = match body.get("type") {
            Some(Value::String(s)) => ItemType::parse(s)?,
            _ => return Err(AppError::BadRequest("field 'type' is required".into())),
        };

        let name = take_string(body, "name")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::BadRequest("field 'name' is required".into()))?;

        let extra_map = fold_extra(body)?;
        let extra = serde_json::to_string(&Value::Object(extra_map))?;

        let sort_date = take_string(body, "sort_date")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(today_date);

        let (image_base64, image_url) = extract_image_sources(body)?;
        if image_base64.is_none() && image_url.is_none() {
            return Err(AppError::BadRequest(
                "an image is required: provide exactly one of 'image_base64' or 'image_url'".into(),
            ));
        }

        Ok(PreparedCreate {
            item_type,
            name,
            url: take_url(body)?,
            aka: normalise_aka(body)?,
            genres: normalise_genres(body)?,
            release_date: take_string(body, "release_date").filter(|s| !s.trim().is_empty()),
            rating: parse_rating(body)?,
            summary: take_string(body, "summary").filter(|s| !s.trim().is_empty()),
            extra,
            sort_date,
            image_base64,
            image_url,
            now: now_iso(),
        })
    }
}

impl PreparedUpdate {
    /// Validate and normalise a partial update payload. Only keys present in
    /// the body are applied. The existing `extra` is merged with new fields by
    /// the repository, so here we only carry an `extra` patch when the body
    /// contains an `extra` object or any recognised top-level field.
    pub fn from_body(body: &Map<String, Value>) -> AppResult<Self> {
        let mut up = PreparedUpdate::default();

        if let Some(v) = body.get("type") {
            if let Value::String(s) = v {
                up.item_type = Some(ItemType::parse(s)?);
            } else {
                return Err(AppError::BadRequest("field 'type' must be a string".into()));
            }
        }

        if body.contains_key("name") {
            let name = take_string(body, "name")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::BadRequest("field 'name' must not be empty".into()))?;
            up.name = Some(name);
        }

        if body.contains_key("url") {
            up.url = Some(take_url(body)?);
        }
        if body.contains_key("aka") {
            up.aka = Some(normalise_aka(body)?);
        }
        if body.contains_key("genres") {
            up.genres = Some(normalise_genres(body)?);
        }
        if body.contains_key("release_date") {
            up.release_date =
                Some(take_string(body, "release_date").filter(|s| !s.trim().is_empty()));
        }
        if body.contains_key("rating") {
            up.rating = Some(parse_rating(body)?);
        }
        if body.contains_key("summary") {
            up.summary = Some(take_string(body, "summary").filter(|s| !s.trim().is_empty()));
        }
        if body.contains_key("sort_date") {
            if let Some(sd) = take_string(body, "sort_date").filter(|s| !s.trim().is_empty()) {
                up.sort_date = Some(sd);
            }
        }

        // Build an extra-patch only if relevant keys are present.
        let has_extra_keys = body.contains_key("extra")
            || EXTRA_FIELD_KEYS.iter().any(|k| body.contains_key(*k));
        if has_extra_keys {
            let extra_map = fold_extra(body)?;
            up.extra = Some(serde_json::to_string(&Value::Object(extra_map))?);
        }

        let (image_base64, image_url) = extract_image_sources(body)?;
        up.image_base64 = image_base64;
        up.image_url = image_url;

        Ok(up)
    }

    /// True if this update changes no columns and carries no image.
    pub fn is_empty(&self) -> bool {
        self.item_type.is_none()
            && self.name.is_none()
            && self.url.is_none()
            && self.aka.is_none()
            && self.genres.is_none()
            && self.release_date.is_none()
            && self.rating.is_none()
            && self.summary.is_none()
            && self.extra.is_none()
            && self.sort_date.is_none()
            && self.image_base64.is_none()
            && self.image_url.is_none()
    }
}
