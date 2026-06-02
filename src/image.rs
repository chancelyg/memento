//! Image acquisition helpers: base64 decoding and remote URL fetching.
//!
//! All blocking work (reqwest blocking client) is expected to be wrapped in
//! `tokio::task::spawn_blocking` by callers.

use std::net::IpAddr;

use base64::{engine::general_purpose::STANDARD, Engine};

use crate::error::{AppError, AppResult};

/// Maximum accepted image size in bytes (10 MiB) — sanity bound.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Default mime applied when one cannot be inferred.
pub const DEFAULT_IMAGE_MIME: &str = "image/jpeg";

/// Sniff the real image type from the leading magic bytes, returning a
/// canonical `image/*` mime. Rejects anything that is not a recognised raster
/// image — this prevents a caller from storing HTML/JS/SVG under an
/// `image/png` label and having it served back with an attacker-chosen
/// `Content-Type` (stored-XSS / content-sniffing defence).
pub fn detect_image_mime(bytes: &[u8]) -> AppResult<String> {
    let mime = match bytes {
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => "image/png",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [0x42, 0x4D, ..] => "image/bmp",
        // RIFF....WEBP
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        _ if is_avif(bytes) => "image/avif",
        _ => {
            return Err(AppError::BadRequest(
                "unrecognised image format (expected jpeg/png/gif/webp/bmp/avif)".into(),
            ))
        }
    };
    Ok(mime.to_string())
}

/// AVIF/HEIF: `....ftyp` box with an `avif`/`avis`/`mif1`/`heic` brand.
fn is_avif(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(&bytes[8..12], b"avif" | b"avis" | b"mif1" | b"heic" | b"heix")
}

/// Decoded image: raw bytes plus mime type.
#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// Decode an `image_base64` value. Accepts both a bare base64 string and a
/// `data:<mime>;base64,<payload>` data URL (the mime is extracted from the
/// latter). Validates size.
pub fn decode_base64_image(input: &str) -> AppResult<DecodedImage> {
    let trimmed = input.trim();

    let (mime, payload) = if let Some(rest) = trimmed.strip_prefix("data:") {
        // data:[<mime>][;base64],<data>
        let comma = rest.find(',').ok_or_else(|| {
            AppError::BadRequest("malformed data URL in 'image_base64'".into())
        })?;
        let meta = &rest[..comma];
        let data = &rest[comma + 1..];
        let mime = meta
            .split(';')
            .next()
            .filter(|m| !m.is_empty())
            .unwrap_or(DEFAULT_IMAGE_MIME)
            .to_string();
        (mime, data)
    } else {
        (DEFAULT_IMAGE_MIME.to_string(), trimmed)
    };

    let bytes = STANDARD
        .decode(payload.trim())
        .map_err(|_| AppError::BadRequest("invalid base64 in 'image_base64'".into()))?;

    validate_size(bytes.len())?;
    if bytes.is_empty() {
        return Err(AppError::BadRequest("decoded image is empty".into()));
    }

    // Trust the content, not the declared mime: verify magic bytes and use the
    // sniffed type. Falls back to the declared mime only if it agrees.
    let sniffed = detect_image_mime(&bytes)?;
    let _ = mime; // declared mime is advisory; sniffed type wins
    Ok(DecodedImage {
        bytes,
        mime: sniffed,
    })
}

/// Fetch a remote image with the blocking reqwest client. MUST be called from
/// within `spawn_blocking`.
pub fn fetch_image_blocking(url: &str) -> AppResult<DecodedImage> {
    // Validate scheme and reject hosts that resolve to private/loopback/
    // link-local addresses (SSRF defence), then fetch with redirects disabled
    // so a public URL cannot redirect into the internal network.
    guard_public_url(url)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AppError::ImageFetch(Box::new(e)))?;

    let resp = client
        .get(url)
        .send()
        .map_err(|e| AppError::ImageFetch(Box::new(e)))?;

    if !resp.status().is_success() {
        return Err(AppError::ImageFetch(Box::new(std::io::Error::other(
            format!("remote returned status {}", resp.status()),
        ))));
    }

    let bytes = resp
        .bytes()
        .map_err(|e| AppError::ImageFetch(Box::new(e)))?
        .to_vec();

    validate_size(bytes.len())?;
    if bytes.is_empty() {
        return Err(AppError::ImageFetch(Box::new(std::io::Error::other(
            "remote image is empty",
        ))));
    }

    // Verify the bytes really are an image and use the sniffed mime.
    let mime = detect_image_mime(&bytes)?;
    Ok(DecodedImage { bytes, mime })
}

/// Reject non-http(s) URLs and any host resolving to a private, loopback,
/// link-local, or otherwise non-global address. Resolves DNS up front so a
/// hostname pointing at an internal IP is also blocked.
fn guard_public_url(url: &str) -> AppResult<()> {
    let parsed = url::Url::parse(url)
        .map_err(|_| AppError::BadRequest("field 'image_url' is not a valid URL".into()))?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(AppError::BadRequest(
                "field 'image_url' must be an http(s) URL".into(),
            ))
        }
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::BadRequest("field 'image_url' has no host".into()))?;
    let port = parsed.port_or_known_default().unwrap_or(80);

    // Resolve the host to its addresses; reject if ANY resolves to a
    // non-global address (conservative — avoids DNS-rebinding bypasses).
    use std::net::ToSocketAddrs;
    let addrs: Vec<IpAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|_| AppError::BadRequest("could not resolve 'image_url' host".into()))?
        .map(|sa| sa.ip())
        .collect();
    if addrs.is_empty() {
        return Err(AppError::BadRequest(
            "could not resolve 'image_url' host".into(),
        ));
    }
    for ip in addrs {
        if !is_global_ip(ip) {
            return Err(AppError::BadRequest(
                "'image_url' host resolves to a non-public address (blocked)".into(),
            ));
        }
    }
    Ok(())
}

/// Conservative "is this a publicly-routable address" check covering loopback,
/// private (RFC 1918), link-local, unspecified, and IPv6 unique-local/loopback.
fn is_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                // CGNAT 100.64.0.0/10
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64))
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                // unique-local fc00::/7
                || (v6.segments()[0] & 0xFE00) == 0xFC00
                // link-local fe80::/10
                || (v6.segments()[0] & 0xFFC0) == 0xFE80)
        }
    }
}

/// Validate raw byte length against [`MAX_IMAGE_BYTES`].
pub fn validate_size(len: usize) -> AppResult<()> {
    if len > MAX_IMAGE_BYTES {
        return Err(AppError::BadRequest(format!(
            "image too large ({len} bytes, max {MAX_IMAGE_BYTES})"
        )));
    }
    Ok(())
}
