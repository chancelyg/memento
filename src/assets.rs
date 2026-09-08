//! Embedded static frontend assets and their HTTP handlers.

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

use crate::config::SiteConfig;
use crate::state::AppState;

/// Compile-time embedded contents of the `static/` directory.
#[derive(RustEmbed)]
#[folder = "static/"]
pub struct StaticAssets;

/// The diary shell contains no private data; all content is fetched after login.
pub async fn diary_handler() -> Response {
    serve_embedded("diary.html")
}

/// Serve `index.html` at `/`, injecting the operator-configured site name,
/// slogan and favicon into the template placeholders.
pub async fn index_handler(State(state): State<AppState>) -> Response {
    match render_index(&state.site) {
        Some(html) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "index unavailable").into_response(),
    }
}

/// Render `index.html` with site-config placeholders substituted. Returns
/// `None` only if the asset is missing or not valid UTF-8 (a build error).
fn render_index(site: &SiteConfig) -> Option<String> {
    let asset = StaticAssets::get("index.html")?;
    let template = std::str::from_utf8(&asset.data).ok()?;
    Some(
        template
            .replace("{{SITE_NAME}}", &escape_text(&site.name))
            .replace("{{SLOGAN}}", &escape_text(&site.slogan))
            // The icon lands in an attribute value (and the default is an inline
            // SVG data URI containing `<`/`>`), so only attribute-escape it.
            .replace("{{ICON}}", &escape_attr(&site.icon)),
    )
}

/// Escape a value for HTML *text* context. Operator-supplied config is trusted,
/// but escaping keeps a stray `<`/`&` from breaking the markup.
fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Escape a value for a double-quoted HTML *attribute* value. Only `&` and `"`
/// need escaping there; `<`/`>` are left intact so an inline SVG data URI works.
fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

/// Serve an arbitrary embedded asset at `/static/{file}`.
pub async fn static_handler(Path(file): Path<String>) -> Response {
    // Reject path traversal attempts.
    if file.contains("..") || file.starts_with('/') {
        return (StatusCode::BAD_REQUEST, "invalid path").into_response();
    }
    serve_embedded(&file)
}

/// Look up an embedded file and return it with the correct Content-Type.
fn serve_embedded(path: &str) -> Response {
    match StaticAssets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                content.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_template_has_placeholders() {
        // Guards against the template and the renderer drifting apart.
        let asset = StaticAssets::get("index.html").expect("index.html embedded");
        let html = std::str::from_utf8(&asset.data).unwrap();
        assert!(
            html.contains("{{SITE_NAME}}"),
            "missing SITE_NAME placeholder"
        );
        assert!(html.contains("{{SLOGAN}}"), "missing SLOGAN placeholder");
        assert!(html.contains("{{ICON}}"), "missing ICON placeholder");
    }

    #[test]
    fn render_index_substitutes_defaults() {
        let html = render_index(&SiteConfig::default()).expect("renders");
        assert!(!html.contains("{{SITE_NAME}}"));
        assert!(!html.contains("{{SLOGAN}}"));
        assert!(!html.contains("{{ICON}}"));
        assert!(html.contains(crate::config::DEFAULT_SITE_NAME));
        assert!(html.contains(crate::config::DEFAULT_SLOGAN));
    }

    #[test]
    fn render_index_substitutes_custom_values() {
        let site = SiteConfig {
            name: "老王的收藏".to_string(),
            slogan: "随心记录".to_string(),
            icon: "https://example.com/f.png".to_string(),
        };
        let html = render_index(&site).expect("renders");
        assert!(html.contains("老王的收藏"));
        assert!(html.contains("随心记录"));
        assert!(html.contains("https://example.com/f.png"));
    }

    #[test]
    fn escape_text_neutralises_markup() {
        assert_eq!(escape_text("a<b>&\"c"), "a&lt;b&gt;&amp;&quot;c");
    }

    #[test]
    fn escape_attr_keeps_angle_brackets() {
        // Inline SVG data URIs rely on `<`/`>` surviving in the attribute.
        assert_eq!(escape_attr("<svg>&\""), "<svg>&amp;&quot;");
    }
}
