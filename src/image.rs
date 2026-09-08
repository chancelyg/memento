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
        // The first 4 bytes are the big-endian box size; require a sane value.
        && u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) >= 8
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
        let comma = rest
            .find(',')
            .ok_or_else(|| AppError::BadRequest("malformed data URL in 'image_base64'".into()))?;
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
    // so a public URL cannot redirect into the internal network. The guard
    // returns the validated host and a pinned address so we connect to the
    // exact IP we checked — closing the DNS-rebinding window that a second,
    // uncontrolled lookup at connect time would open.
    let (host, addr) = guard_public_url(url)?;

    // The client is built per request because the `.resolve()` pin is
    // host-specific: a shared/global client would reintroduce the rebinding
    // risk by letting reqwest re-resolve the host on a later request.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .resolve(&host, addr)
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
/// hostname pointing at an internal IP is also blocked, and returns the
/// validated host together with a pinned `SocketAddr` so the caller can
/// connect to the exact address that was checked (no second DNS lookup).
fn guard_public_url(url: &str) -> AppResult<(String, std::net::SocketAddr)> {
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
    let socket_addrs: Vec<std::net::SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|_| AppError::BadRequest("could not resolve 'image_url' host".into()))?
        .collect();
    if socket_addrs.is_empty() {
        return Err(AppError::BadRequest(
            "could not resolve 'image_url' host".into(),
        ));
    }
    for sa in &socket_addrs {
        let ip: IpAddr = sa.ip();
        if !is_global_ip(ip) {
            return Err(AppError::BadRequest(
                "'image_url' host resolves to a non-public address (blocked)".into(),
            ));
        }
    }

    // Pin the first resolved address: every address was validated as global
    // above (we reject when ANY is non-global), so the first is safe to use.
    let pinned = socket_addrs[0];
    Ok((host.to_string(), pinned))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// A valid 1x1 PNG encoded as base64 (decodes to a real PNG).
    const PNG_1X1_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M8AAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

    /// Assert that an `AppResult` is `Err(AppError::BadRequest(_))`.
    fn assert_bad_request<T: std::fmt::Debug>(result: AppResult<T>) {
        match result {
            Err(AppError::BadRequest(_)) => {}
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    // ---- detect_image_mime ------------------------------------------------

    #[test]
    fn detect_mime_jpeg() {
        // FF D8 FF marker plus a trailing byte.
        let bytes = [0xFF, 0xD8, 0xFF, 0xE0];
        assert_eq!(detect_image_mime(&bytes).unwrap(), "image/jpeg");
    }

    #[test]
    fn detect_mime_png() {
        let bytes = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00];
        assert_eq!(detect_image_mime(&bytes).unwrap(), "image/png");
    }

    #[test]
    fn detect_mime_gif() {
        // "GIF8" prefix (covers GIF87a and GIF89a).
        let bytes = *b"GIF89a";
        assert_eq!(detect_image_mime(&bytes).unwrap(), "image/gif");
    }

    #[test]
    fn detect_mime_bmp() {
        // "BM" prefix.
        let bytes = [0x42, 0x4D, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(detect_image_mime(&bytes).unwrap(), "image/bmp");
    }

    #[test]
    fn detect_mime_webp() {
        // RIFF....WEBP container.
        let bytes = [
            b'R', b'I', b'F', b'F', 0x10, 0x00, 0x00, 0x00, b'W', b'E', b'B', b'P', b'V', b'P',
        ];
        assert_eq!(detect_image_mime(&bytes).unwrap(), "image/webp");
    }

    #[test]
    fn detect_mime_unknown_is_bad_request() {
        let bytes = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        assert_bad_request(detect_image_mime(&bytes));
    }

    #[test]
    fn detect_mime_empty_is_bad_request() {
        assert_bad_request(detect_image_mime(&[]));
    }

    #[test]
    fn detect_mime_riff_without_webp_is_bad_request() {
        // RIFF container that is not WEBP (e.g. a WAV) must be rejected.
        let bytes = [
            b'R', b'I', b'F', b'F', 0x10, 0x00, 0x00, 0x00, b'W', b'A', b'V', b'E',
        ];
        assert_bad_request(detect_image_mime(&bytes));
    }

    // ---- is_avif ----------------------------------------------------------

    /// Build a minimal `ftyp` box with the given 4-byte brand.
    fn ftyp_box(brand: &[u8; 4]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&20u32.to_be_bytes()); // box size (>= 8)
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(brand);
        v.extend_from_slice(&[0u8; 8]); // padding to make a plausible box
        v
    }

    #[test]
    fn is_avif_true_for_avif_brand() {
        assert!(is_avif(&ftyp_box(b"avif")));
    }

    #[test]
    fn is_avif_true_for_other_heif_brands() {
        for brand in [b"avis", b"mif1", b"heic", b"heix"] {
            assert!(is_avif(&ftyp_box(brand)), "brand {brand:?} should match");
        }
        // And detect_image_mime should surface it as image/avif.
        assert_eq!(detect_image_mime(&ftyp_box(b"avif")).unwrap(), "image/avif");
    }

    #[test]
    fn is_avif_false_for_too_short() {
        assert!(!is_avif(&[]));
        assert!(!is_avif(b"ftyp")); // only 4 bytes
        assert!(!is_avif(&[
            0, 0, 0, 20, b'f', b't', b'y', b'p', b'a', b'v', b'i'
        ])); // 11 bytes
    }

    #[test]
    fn is_avif_false_for_wrong_box_tag() {
        // 12 bytes but the box tag is not "ftyp".
        let mut v = 20u32.to_be_bytes().to_vec();
        v.extend_from_slice(b"moov");
        v.extend_from_slice(b"avif");
        assert!(!is_avif(&v));
    }

    #[test]
    fn is_avif_false_for_unknown_brand() {
        assert!(!is_avif(&ftyp_box(b"qt  ")));
    }

    #[test]
    fn is_avif_false_for_tiny_box_size() {
        // ftyp present and brand valid, but the declared box size (< 8) is bogus.
        let mut v = 4u32.to_be_bytes().to_vec();
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(b"avif");
        assert!(!is_avif(&v));
    }

    // ---- decode_base64_image ---------------------------------------------

    #[test]
    fn decode_bare_png_base64() {
        let decoded = decode_base64_image(PNG_1X1_B64).unwrap();
        assert_eq!(decoded.mime, "image/png");
        assert!(!decoded.bytes.is_empty());
        // Verify the PNG magic bytes really landed in the output.
        assert_eq!(
            &decoded.bytes[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
    }

    #[test]
    fn decode_data_url_png() {
        let url = format!("data:image/png;base64,{PNG_1X1_B64}");
        let decoded = decode_base64_image(&url).unwrap();
        // Sniffed type wins over (here, agreeing) declared type.
        assert_eq!(decoded.mime, "image/png");
    }

    #[test]
    fn decode_data_url_mime_is_advisory_sniffed_wins() {
        // Declare a lying mime; the PNG payload must still be sniffed as png.
        let url = format!("data:image/jpeg;base64,{PNG_1X1_B64}");
        let decoded = decode_base64_image(&url).unwrap();
        assert_eq!(decoded.mime, "image/png");
    }

    #[test]
    fn decode_trims_surrounding_whitespace() {
        let padded = format!("  \n{PNG_1X1_B64}\n  ");
        let decoded = decode_base64_image(&padded).unwrap();
        assert_eq!(decoded.mime, "image/png");
    }

    #[test]
    fn decode_invalid_base64_is_bad_request() {
        // '!' is not in the standard base64 alphabet.
        assert_bad_request(decode_base64_image("!!!not-base64!!!"));
    }

    #[test]
    fn decode_empty_after_decode_is_bad_request() {
        // Empty string decodes to zero bytes -> "decoded image is empty".
        assert_bad_request(decode_base64_image(""));
    }

    #[test]
    fn decode_non_image_bytes_is_bad_request() {
        // base64 of "hello" decodes to 5 ASCII bytes -> fails magic-byte sniff.
        let hello = STANDARD.encode("hello");
        assert_bad_request(decode_base64_image(&hello));
    }

    #[test]
    fn decode_data_url_without_comma_is_bad_request() {
        assert_bad_request(decode_base64_image("data:image/png;base64"));
    }

    // ---- validate_size ----------------------------------------------------

    #[test]
    fn validate_size_within_limit_ok() {
        assert!(validate_size(0).is_ok());
        assert!(validate_size(1).is_ok());
        assert!(validate_size(MAX_IMAGE_BYTES).is_ok());
        assert!(validate_size(MAX_IMAGE_BYTES - 1).is_ok());
    }

    #[test]
    fn validate_size_over_limit_is_bad_request() {
        assert_bad_request(validate_size(MAX_IMAGE_BYTES + 1));
    }

    // ---- is_global_ip -----------------------------------------------------

    #[test]
    fn global_ipv4_is_true() {
        assert!(is_global_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn non_global_ipv4_is_false() {
        let cases = [
            Ipv4Addr::new(127, 0, 0, 1),       // loopback
            Ipv4Addr::new(10, 0, 0, 1),        // private RFC1918
            Ipv4Addr::new(192, 168, 1, 1),     // private RFC1918
            Ipv4Addr::new(169, 254, 0, 1),     // link-local
            Ipv4Addr::new(100, 64, 0, 1),      // CGNAT 100.64.0.0/10
            Ipv4Addr::new(0, 0, 0, 0),         // unspecified
            Ipv4Addr::new(255, 255, 255, 255), // broadcast
        ];
        for ip in cases {
            assert!(!is_global_ip(IpAddr::V4(ip)), "{ip} should be non-global");
        }
    }

    #[test]
    fn cgnat_boundaries() {
        // 100.64.0.0/10 spans 100.64.0.0 .. 100.127.255.255.
        assert!(!is_global_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0))));
        assert!(!is_global_ip(IpAddr::V4(Ipv4Addr::new(100, 127, 255, 255))));
        // 100.128.x.x is OUTSIDE the CGNAT block -> global.
        assert!(is_global_ip(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))));
        // 100.63.x.x is below the block -> global.
        assert!(is_global_ip(IpAddr::V4(Ipv4Addr::new(100, 63, 0, 1))));
    }

    #[test]
    fn ipv6_loopback_and_local_are_false() {
        assert!(!is_global_ip(IpAddr::V6(Ipv6Addr::LOCALHOST))); // ::1
        assert!(!is_global_ip(IpAddr::V6(Ipv6Addr::UNSPECIFIED))); // ::
                                                                   // unique-local fc00::/7
        assert!(!is_global_ip(IpAddr::V6(
            "fc00::".parse::<Ipv6Addr>().unwrap()
        )));
        assert!(!is_global_ip(IpAddr::V6(
            "fd12:3456::1".parse::<Ipv6Addr>().unwrap()
        )));
        // link-local fe80::/10
        assert!(!is_global_ip(IpAddr::V6(
            "fe80::1".parse::<Ipv6Addr>().unwrap()
        )));
    }

    #[test]
    fn global_ipv6_is_true() {
        // Cloudflare public resolver 2606:4700:4700::1111.
        let ip: Ipv6Addr = "2606:4700:4700::1111".parse().unwrap();
        assert!(is_global_ip(IpAddr::V6(ip)));
    }

    // ---- guard_public_url (network-free cases only) -----------------------

    #[test]
    fn guard_rejects_non_http_scheme() {
        assert_bad_request(guard_public_url("ftp://example.com/x"));
    }

    #[test]
    fn guard_rejects_loopback_literal() {
        // IP literal -> no external DNS needed.
        assert_bad_request(guard_public_url("http://127.0.0.1/x"));
    }

    #[test]
    fn guard_rejects_private_literal() {
        assert_bad_request(guard_public_url("http://10.0.0.1/x"));
    }

    #[test]
    fn guard_rejects_unparseable_url() {
        assert_bad_request(guard_public_url("not a url"));
    }
}
