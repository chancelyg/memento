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

/// Take a string field from the body. A present `String` is returned as-is;
/// absent or `Null` yields `None`; any other JSON type is rejected (no silent
/// coercion of numbers/bools into strings).
fn take_string(body: &Map<String, Value>, key: &str) -> AppResult<Option<String>> {
    match body.get(key) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(AppError::BadRequest(format!(
            "field '{key}' must be a string"
        ))),
    }
}

/// Take and validate the optional `url` field: must be an http(s) URL when
/// present (rejects `javascript:`/`data:` schemes — defence against stored XSS
/// when the value is later used as a link `href`).
fn take_url(body: &Map<String, Value>) -> AppResult<Option<String>> {
    match take_string(body, "url")?.map(|s| s.trim().to_string()) {
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
            return Err(AppError::BadRequest(
                "field 'extra' must be an object".into(),
            ));
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
        Some(_) => Err(AppError::BadRequest(
            "field 'rating' must be a number".into(),
        )),
    }
}

/// At most one image source may be supplied.
fn extract_image_sources(body: &Map<String, Value>) -> AppResult<(Option<String>, Option<String>)> {
    let image_base64 = take_string(body, "image_base64")?.filter(|s| !s.trim().is_empty());
    let image_url = take_string(body, "image_url")?.filter(|s| !s.trim().is_empty());
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

        let name = take_string(body, "name")?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::BadRequest("field 'name' is required".into()))?;

        let extra_map = fold_extra(body)?;
        let extra = serde_json::to_string(&Value::Object(extra_map))?;

        let sort_date = take_string(body, "sort_date")?
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
            release_date: take_string(body, "release_date")?.filter(|s| !s.trim().is_empty()),
            rating: parse_rating(body)?,
            summary: take_string(body, "summary")?.filter(|s| !s.trim().is_empty()),
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
            let name = take_string(body, "name")?
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
                Some(take_string(body, "release_date")?.filter(|s| !s.trim().is_empty()));
        }
        if body.contains_key("rating") {
            up.rating = Some(parse_rating(body)?);
        }
        if body.contains_key("summary") {
            up.summary = Some(take_string(body, "summary")?.filter(|s| !s.trim().is_empty()));
        }
        if body.contains_key("sort_date") {
            if let Some(sd) = take_string(body, "sort_date")?.filter(|s| !s.trim().is_empty()) {
                up.sort_date = Some(sd);
            }
        }

        // Build an extra-patch only if relevant keys are present.
        let has_extra_keys =
            body.contains_key("extra") || EXTRA_FIELD_KEYS.iter().any(|k| body.contains_key(*k));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a `Map<String, Value>` body from a JSON literal.
    fn body(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    /// Assert that an `AppResult` is an `AppError::BadRequest`.
    fn assert_bad_request<T: std::fmt::Debug>(res: AppResult<T>) {
        match res {
            Err(AppError::BadRequest(_)) => {}
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    // ----- ItemType::parse -----------------------------------------------

    #[test]
    fn parse_game_variants() {
        assert_eq!(ItemType::parse("game").unwrap(), ItemType::Game);
        assert_eq!(ItemType::parse("游戏").unwrap(), ItemType::Game);
    }

    #[test]
    fn parse_movie_variants() {
        assert_eq!(ItemType::parse("movie").unwrap(), ItemType::Movie);
        assert_eq!(ItemType::parse("电影").unwrap(), ItemType::Movie);
    }

    #[test]
    fn parse_book_variants() {
        assert_eq!(ItemType::parse("book").unwrap(), ItemType::Book);
        assert_eq!(ItemType::parse("图书").unwrap(), ItemType::Book);
    }

    #[test]
    fn parse_unknown_is_err() {
        assert_bad_request(ItemType::parse("widget"));
    }

    // ----- ItemType::parse_filter ----------------------------------------

    #[test]
    fn parse_filter_none_empty_all_yields_no_filter() {
        assert_eq!(ItemType::parse_filter(None).unwrap(), None);
        assert_eq!(ItemType::parse_filter(Some("")).unwrap(), None);
        assert_eq!(ItemType::parse_filter(Some("all")).unwrap(), None);
        assert_eq!(ItemType::parse_filter(Some("全部")).unwrap(), None);
    }

    #[test]
    fn parse_filter_specific_type() {
        assert_eq!(
            ItemType::parse_filter(Some("game")).unwrap(),
            Some(ItemType::Game)
        );
    }

    #[test]
    fn parse_filter_bad_is_err() {
        assert_bad_request(ItemType::parse_filter(Some("widget")));
    }

    // ----- PreparedCreate::from_body -------------------------------------

    /// A minimal valid create body (type + name + one image source).
    fn minimal_valid_create() -> Map<String, Value> {
        body(json!({
            "type": "game",
            "name": "Hollow Knight",
            "image_url": "https://example.com/hk.png"
        }))
    }

    #[test]
    fn create_minimal_valid_ok() {
        let prepared = PreparedCreate::from_body(&minimal_valid_create()).unwrap();
        assert_eq!(prepared.item_type, ItemType::Game);
        assert_eq!(prepared.name, "Hollow Knight");
        assert_eq!(
            prepared.image_url.as_deref(),
            Some("https://example.com/hk.png")
        );
        assert!(prepared.image_base64.is_none());
    }

    #[test]
    fn create_missing_type_is_err() {
        let b = body(json!({
            "name": "Hollow Knight",
            "image_url": "https://example.com/hk.png"
        }));
        assert_bad_request(PreparedCreate::from_body(&b));
    }

    #[test]
    fn create_missing_name_is_err() {
        let b = body(json!({
            "type": "game",
            "image_url": "https://example.com/hk.png"
        }));
        assert_bad_request(PreparedCreate::from_body(&b));
    }

    #[test]
    fn create_missing_image_is_err() {
        // New required-image rule: neither image_base64 nor image_url present.
        let b = body(json!({
            "type": "game",
            "name": "Hollow Knight"
        }));
        assert_bad_request(PreparedCreate::from_body(&b));
    }

    #[test]
    fn create_both_image_sources_is_err() {
        let b = body(json!({
            "type": "game",
            "name": "Hollow Knight",
            "image_base64": "AAAA",
            "image_url": "https://example.com/hk.png"
        }));
        assert_bad_request(PreparedCreate::from_body(&b));
    }

    #[test]
    fn create_rating_out_of_range_is_err() {
        let b = body(json!({
            "type": "game",
            "name": "Hollow Knight",
            "image_url": "https://example.com/hk.png",
            "rating": 11
        }));
        assert_bad_request(PreparedCreate::from_body(&b));
    }

    #[test]
    fn create_rating_in_range_ok() {
        let b = body(json!({
            "type": "game",
            "name": "Hollow Knight",
            "image_url": "https://example.com/hk.png",
            "rating": 9.5
        }));
        let prepared = PreparedCreate::from_body(&b).unwrap();
        assert_eq!(prepared.rating, Some(9.5));
    }

    #[test]
    fn create_non_string_name_is_err() {
        // Strict take_string: a JSON number is rejected, not coerced.
        let b = body(json!({
            "type": "game",
            "name": 42,
            "image_url": "https://example.com/hk.png"
        }));
        assert_bad_request(PreparedCreate::from_body(&b));
    }

    #[test]
    fn create_non_http_url_is_err() {
        let b = body(json!({
            "type": "game",
            "name": "Hollow Knight",
            "image_url": "https://example.com/hk.png",
            "url": "javascript:alert(1)"
        }));
        assert_bad_request(PreparedCreate::from_body(&b));
    }

    #[test]
    fn create_folds_type_specific_fields_into_extra() {
        let b = body(json!({
            "type": "game",
            "name": "Hollow Knight",
            "image_url": "https://example.com/hk.png",
            "developer": "Team Cherry",
            "platforms": ["PC", "Switch"]
        }));
        let prepared = PreparedCreate::from_body(&b).unwrap();
        assert!(
            prepared.extra.contains("developer"),
            "extra should fold developer: {}",
            prepared.extra
        );
        assert!(
            prepared.extra.contains("Team Cherry"),
            "extra should contain developer value: {}",
            prepared.extra
        );
        assert!(
            prepared.extra.contains("platforms"),
            "extra should fold platforms: {}",
            prepared.extra
        );
    }

    // ----- PreparedUpdate::from_body -------------------------------------

    #[test]
    fn update_empty_body_is_empty() {
        let b = body(json!({}));
        let up = PreparedUpdate::from_body(&b).unwrap();
        assert!(up.is_empty());
    }

    #[test]
    fn update_only_rating_is_not_empty() {
        let b = body(json!({ "rating": 7 }));
        let up = PreparedUpdate::from_body(&b).unwrap();
        assert!(!up.is_empty());
        assert_eq!(up.rating, Some(Some(7.0)));
    }

    #[test]
    fn update_explicit_extra_key_merges() {
        let b = body(json!({
            "extra": { "edition": "Collector's" }
        }));
        let up = PreparedUpdate::from_body(&b).unwrap();
        let patch = up.extra.expect("extra patch should be present");
        assert!(
            patch.contains("edition"),
            "extra patch should contain the explicit key: {patch}"
        );
        assert!(
            patch.contains("Collector's"),
            "extra patch should contain the value: {patch}"
        );
    }

    #[test]
    fn update_non_string_name_is_err() {
        let b = body(json!({ "name": 42 }));
        assert_bad_request(PreparedUpdate::from_body(&b));
    }

    // ----- normalise_aka / normalise_genres ------------------------------

    #[test]
    fn normalise_aka_accepts_string_and_array() {
        let s = normalise_aka(&body(json!({ "aka": "Alt Name" }))).unwrap();
        assert_eq!(s.as_deref(), Some("\"Alt Name\""));

        let a = normalise_aka(&body(json!({ "aka": ["A", "B"] }))).unwrap();
        assert_eq!(a.as_deref(), Some("[\"A\",\"B\"]"));
    }

    #[test]
    fn normalise_genres_string_becomes_single_element_array() {
        let g = normalise_genres(&body(json!({ "genres": "Metroidvania" }))).unwrap();
        assert_eq!(g.as_deref(), Some("[\"Metroidvania\"]"));

        let arr = normalise_genres(&body(json!({ "genres": ["RPG", "Indie"] }))).unwrap();
        assert_eq!(arr.as_deref(), Some("[\"RPG\",\"Indie\"]"));
    }
}
