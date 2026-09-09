//! Typed YAML settings and atomic persistence.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

use crate::config::{SiteConfig, DEFAULT_ICON, DEFAULT_SITE_NAME, DEFAULT_SLOGAN};
use crate::error::{AppError, AppResult};

const SETTINGS_VERSION: u32 = 1;
const MAX_ICON_BYTES: usize = 256 * 1024;
const MAX_SETTINGS_FILE_BYTES: u64 = 512 * 1024;
static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub version: u32,
    #[serde(default)]
    pub site: SiteSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            site: SiteSettings::default(),
        }
    }
}

impl AppSettings {
    pub fn validate(&self) -> AppResult<Self> {
        if self.version != SETTINGS_VERSION {
            return Err(AppError::BadRequest("settings version must be 1".into()));
        }

        Ok(Self {
            version: SETTINGS_VERSION,
            site: self.site.validate()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SiteSettings {
    pub name: String,
    pub slogan: String,
    pub icon: String,
}

impl Default for SiteSettings {
    fn default() -> Self {
        Self {
            name: DEFAULT_SITE_NAME.to_string(),
            slogan: DEFAULT_SLOGAN.to_string(),
            icon: DEFAULT_ICON.to_string(),
        }
    }
}

impl SiteSettings {
    pub fn validate(&self) -> AppResult<Self> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return Err(AppError::BadRequest(
                "site name must contain 1 to 80 characters".into(),
            ));
        }

        let slogan = self.slogan.trim();
        if slogan.chars().count() > 200 {
            return Err(AppError::BadRequest(
                "site slogan must contain at most 200 characters".into(),
            ));
        }

        let icon = self.icon.trim();
        validate_icon(icon)?;

        Ok(Self {
            name: name.to_string(),
            slogan: slogan.to_string(),
            icon: icon.to_string(),
        })
    }
}

impl From<SiteConfig> for SiteSettings {
    fn from(site: SiteConfig) -> Self {
        Self {
            name: site.name,
            slogan: site.slogan,
            icon: site.icon,
        }
    }
}

fn validate_icon(icon: &str) -> AppResult<()> {
    if icon == DEFAULT_ICON {
        return Ok(());
    }
    if icon.starts_with("data:") {
        return validate_data_icon(icon);
    }
    if icon.len() > 2048 {
        return Err(AppError::BadRequest(
            "site icon must contain at most 2048 bytes".into(),
        ));
    }

    let url = url::Url::parse(icon).map_err(|_| {
        AppError::BadRequest("site icon must be a valid HTTPS URL or image data URI".into())
    })?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(AppError::BadRequest(
            "site icon must be an HTTPS URL with a host and no userinfo".into(),
        ));
    }
    Ok(())
}

fn validate_data_icon(icon: &str) -> AppResult<()> {
    let (metadata, payload) = icon
        .split_once(',')
        .ok_or_else(|| AppError::BadRequest("site icon data URI is malformed".into()))?;
    let declared_mime = metadata
        .strip_prefix("data:")
        .and_then(|value| value.strip_suffix(";base64"))
        .filter(|mime| {
            matches!(
                *mime,
                "image/png"
                    | "image/jpeg"
                    | "image/gif"
                    | "image/webp"
                    | "image/bmp"
                    | "image/avif"
            )
        })
        .ok_or_else(|| {
            AppError::BadRequest("site icon data URI has an unsupported MIME type".into())
        })?;

    let decoded = STANDARD
        .decode(payload)
        .map_err(|_| AppError::BadRequest("site icon data URI contains invalid base64".into()))?;
    if decoded.len() > MAX_ICON_BYTES {
        return Err(AppError::BadRequest(
            "site icon decoded image exceeds 256 KiB".into(),
        ));
    }
    let detected = crate::image::detect_image_mime(&decoded)?;
    if detected != declared_mime {
        return Err(AppError::BadRequest(
            "site icon MIME type does not match its image data".into(),
        ));
    }
    Ok(())
}

pub struct SettingsStore {
    path: Option<PathBuf>,
    write_lock: Mutex<()>,
    current: RwLock<AppSettings>,
}

impl SettingsStore {
    pub fn in_memory() -> Self {
        Self::in_memory_with(AppSettings::default())
    }

    pub fn in_memory_with(settings: AppSettings) -> Self {
        Self {
            path: None,
            write_lock: Mutex::new(()),
            current: RwLock::new(settings),
        }
    }

    pub fn load_or_create(path: impl Into<PathBuf>, legacy: Option<SiteConfig>) -> AppResult<Self> {
        let path = path.into();
        let settings = load_or_create_settings(&path, legacy)?;
        Ok(Self {
            path: Some(path),
            write_lock: Mutex::new(()),
            current: RwLock::new(settings),
        })
    }

    pub fn snapshot(&self) -> AppSettings {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn replace_site(self: &Arc<Self>, site: SiteSettings) -> AppResult<AppSettings> {
        let store = Arc::clone(self);
        tokio::task::spawn_blocking(move || store.replace_site_sync(site)).await?
    }

    fn replace_site_sync(&self, site: SiteSettings) -> AppResult<AppSettings> {
        let _serial = self
            .write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = self
            .current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let candidate = AppSettings { site, ..previous }.validate()?;
        let yaml = serialize_settings(&candidate)?;

        if let Some(path) = &self.path {
            atomic_write(path, yaml.as_bytes())?;
        }
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = candidate.clone();
        if let Some(path) = &self.path {
            sync_parent_best_effort(settings_parent(path));
        }
        Ok(candidate)
    }
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self::in_memory()
    }
}

fn load_or_create_settings(path: &Path, legacy: Option<SiteConfig>) -> AppResult<AppSettings> {
    if path.exists() {
        if fs::metadata(path).map_err(internal_error)?.len() > MAX_SETTINGS_FILE_BYTES {
            return Err(AppError::BadRequest(
                "YAML settings file exceeds 512 KiB".into(),
            ));
        }
        let yaml = fs::read_to_string(path)
            .map_err(|_| AppError::BadRequest("cannot read the YAML settings file".into()))?;
        return parse_settings(&yaml)
            .map_err(|_| AppError::BadRequest("invalid YAML settings file".into()));
    }

    let parent = settings_parent(path);
    if !parent.is_dir() {
        return Err(AppError::BadRequest(
            "YAML settings parent directory does not exist".into(),
        ));
    }

    let settings = AppSettings {
        version: SETTINGS_VERSION,
        site: seed_legacy_site(legacy),
    };
    let yaml = serialize_settings(&settings)?;
    atomic_write(path, yaml.as_bytes())?;
    sync_parent_best_effort(parent);
    Ok(settings)
}

fn parse_settings(yaml: &str) -> Result<AppSettings, Box<dyn std::error::Error + Send + Sync>> {
    let parsed: AppSettings = noyalib::from_str_strict(yaml)?;
    parsed
        .validate()
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
}

fn serialize_settings(settings: &AppSettings) -> AppResult<String> {
    noyalib::to_string(settings).map_err(internal_error)
}

fn seed_legacy_site(legacy: Option<SiteConfig>) -> SiteSettings {
    let Some(legacy) = legacy else {
        return SiteSettings::default();
    };
    let defaults = SiteSettings::default();
    SiteSettings {
        name: validate_legacy_field("name", legacy.name, &defaults.name, |value| {
            SiteSettings {
                name: value,
                ..defaults.clone()
            }
            .validate()
            .map(|site| site.name)
        }),
        slogan: validate_legacy_field("slogan", legacy.slogan, &defaults.slogan, |value| {
            SiteSettings {
                slogan: value,
                ..defaults.clone()
            }
            .validate()
            .map(|site| site.slogan)
        }),
        icon: validate_legacy_field("icon", legacy.icon, &defaults.icon, |value| {
            SiteSettings {
                icon: value,
                ..defaults.clone()
            }
            .validate()
            .map(|site| site.icon)
        }),
    }
}

fn validate_legacy_field(
    field: &'static str,
    value: String,
    default: &str,
    validate: impl FnOnce(String) -> AppResult<String>,
) -> String {
    if value.trim().is_empty() {
        return default.to_string();
    }
    match validate(value) {
        Ok(value) => value,
        Err(_) => {
            tracing::warn!(field, "invalid legacy site setting; using default");
            default.to_string()
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let parent = settings_parent(path);
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings");
    let mut last_collision = None;

    for _ in 0..16 {
        let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".{filename}.{}.{id}.tmp", std::process::id()));
        match open_temp_file(&temp_path) {
            Ok(mut file) => {
                let result = (|| -> std::io::Result<()> {
                    file.write_all(bytes)?;
                    file.sync_all()?;
                    drop(file);
                    fs::rename(&temp_path, path)?;
                    Ok(())
                })();
                if let Err(error) = result {
                    let _ = fs::remove_file(&temp_path);
                    return Err(internal_error(error));
                }
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_collision = Some(error);
            }
            Err(error) => return Err(internal_error(error)),
        }
    }

    Err(internal_error(last_collision.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate settings temporary file",
        )
    })))
}

fn open_temp_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn settings_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_parent_best_effort(parent: &Path) {
    if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
        tracing::warn!(error = %error, "could not sync settings directory");
    }
}

fn internal_error(error: impl std::error::Error + Send + Sync + 'static) -> AppError {
    AppError::Internal(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M8AAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

    #[test]
    fn yaml_defaults_missing_site_and_fields() {
        let settings = parse_settings("version: 1\n").unwrap();
        assert_eq!(settings, AppSettings::default());

        let settings = parse_settings("version: 1\nsite:\n  name: custom\n").unwrap();
        assert_eq!(settings.site.name, "custom");
        assert_eq!(settings.site.slogan, DEFAULT_SLOGAN);
        assert_eq!(settings.site.icon, DEFAULT_ICON);
    }

    #[test]
    fn yaml_rejects_unknown_fields_and_wrong_version() {
        assert!(parse_settings("version: 1\nunknown: true\n").is_err());
        assert!(parse_settings("version: 1\nsite:\n  unknown: true\n").is_err());
        assert!(parse_settings("version: 2\n").is_err());
    }

    #[test]
    fn site_validation_normalizes_and_checks_fields() {
        let site = SiteSettings {
            name: "  collection  ".into(),
            slogan: "  hello  ".into(),
            icon: "  https://example.com/icon.png  ".into(),
        }
        .validate()
        .unwrap();
        assert_eq!(site.name, "collection");
        assert_eq!(site.slogan, "hello");
        assert_eq!(site.icon, "https://example.com/icon.png");

        assert!(SiteSettings {
            name: " ".into(),
            ..SiteSettings::default()
        }
        .validate()
        .is_err());
        assert!(SiteSettings {
            slogan: "x".repeat(201),
            ..SiteSettings::default()
        }
        .validate()
        .is_err());
        assert!(SiteSettings {
            icon: "https://user@example.com/icon.png".into(),
            ..SiteSettings::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn data_icon_requires_strict_base64_matching_mime() {
        let valid = SiteSettings {
            icon: format!("data:image/png;base64,{PNG}"),
            ..SiteSettings::default()
        };
        assert!(valid.validate().is_ok());
        assert!(SiteSettings {
            icon: format!("data:image/jpeg;base64,{PNG}"),
            ..SiteSettings::default()
        }
        .validate()
        .is_err());
        assert!(SiteSettings {
            icon: "data:image/svg+xml;base64,PHN2Zz4=".into(),
            ..SiteSettings::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn creates_first_file_from_legacy_with_per_field_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.yaml");
        let store = SettingsStore::load_or_create(
            &path,
            Some(SiteConfig {
                name: "  legacy  ".into(),
                slogan: "kept".into(),
                icon: "javascript:alert(1)".into(),
            }),
        )
        .unwrap();
        assert_eq!(store.snapshot().site.name, "legacy");
        assert_eq!(store.snapshot().site.slogan, "kept");
        assert_eq!(store.snapshot().site.icon, DEFAULT_ICON);
        assert_eq!(
            parse_settings(&fs::read_to_string(path).unwrap()).unwrap(),
            store.snapshot()
        );
    }

    #[test]
    fn existing_invalid_file_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.yaml");
        let original = "version: 2\nsecret: preserved\n";
        fs::write(&path, original).unwrap();
        assert!(SettingsStore::load_or_create(&path, None).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_settings_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.yaml");
        SettingsStore::load_or_create(&path, None).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn oversized_existing_file_is_rejected_without_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.yaml");
        let original = "x".repeat(MAX_SETTINGS_FILE_BYTES as usize + 1);
        fs::write(&path, &original).unwrap();
        assert!(SettingsStore::load_or_create(&path, None).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[tokio::test]
    async fn replace_site_persists_and_updates_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.yaml");
        let store = Arc::new(SettingsStore::load_or_create(&path, None).unwrap());
        let replacement = SiteSettings {
            name: "new name".into(),
            slogan: String::new(),
            icon: "https://example.com/icon.png".into(),
        };

        let saved = store.replace_site(replacement.clone()).await.unwrap();
        assert_eq!(saved.site, replacement);
        assert_eq!(store.snapshot(), saved);
        assert_eq!(
            parse_settings(&fs::read_to_string(path).unwrap()).unwrap(),
            saved
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_write_finishes_after_waiter_is_cancelled() {
        use std::sync::Barrier;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.yaml");
        let store = Arc::new(SettingsStore::load_or_create(&path, None).unwrap());
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_store = Arc::clone(&store);
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let task = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                worker_started.wait();
                worker_release.wait();
                worker_store.replace_site_sync(SiteSettings {
                    name: "cancel-safe".into(),
                    ..SiteSettings::default()
                })
            })
            .await
        });

        tokio::task::spawn_blocking(move || started.wait())
            .await
            .unwrap();
        task.abort();
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .unwrap();

        for _ in 0..100 {
            if store.snapshot().site.name == "cancel-safe" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(store.snapshot().site.name, "cancel-safe");
        assert_eq!(
            parse_settings(&fs::read_to_string(path).unwrap())
                .unwrap()
                .site
                .name,
            "cancel-safe"
        );
    }

    #[test]
    fn failed_replace_does_not_change_file_or_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.yaml");
        let store = SettingsStore::load_or_create(&path, None).unwrap();
        let before_file = fs::read_to_string(&path).unwrap();
        let before_snapshot = store.snapshot();

        assert!(store
            .replace_site_sync(SiteSettings {
                name: String::new(),
                ..SiteSettings::default()
            })
            .is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), before_file);
        assert_eq!(store.snapshot(), before_snapshot);
    }
}
