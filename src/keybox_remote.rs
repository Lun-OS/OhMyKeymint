//! Remote Keybox auto-fetch settings shared by the WebUI commands and the
//! module boot script.
//!
//! The settings live in a tiny line-oriented file under the existing OMK data
//! directory so the boot script can gate on the feature without a second
//! parser. The download itself always runs in a short-lived helper process;
//! the KeyMint daemon only observes the replaced `keybox.xml` through its
//! existing file watcher.

use std::{fs, path::Path, time::Duration};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use ureq::http::Uri;

use crate::{
    keybox::{self, KEYBOX_PATH},
    webui_activity, webui_http,
};

pub const DEFAULT_KEYBOX_REMOTE_URL: &str =
    "https://raw.githubusercontent.com/Yurii0307/yurikey/main/key";
pub const DEFAULT_KEYBOX_REMOTE_PROXY: &str = "https://gh-proxy.org/$url";

const SETTINGS_PATH: &str = "/data/misc/keystore/omk/data/keybox_remote.conf";
const URL_PLACEHOLDER: &str = "$url";
const MAX_SETTING_BYTES: usize = 2048;
const MAX_REMOTE_KEYBOX_PAYLOAD_BYTES: usize = 256 * 1024;
const REMOTE_KEYBOX_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_KEYBOX_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REDIRECTS: usize = 3;

/// Refresh interval bounds and default, in minutes.
pub const MIN_INTERVAL_MINUTES: u32 = 15;
pub const MAX_INTERVAL_MINUTES: u32 = 7 * 24 * 60;
pub const DEFAULT_INTERVAL_MINUTES: u32 = 6 * 60;

const FETCH_RESULT_INSTALLED: &str = "installed";
const FETCH_RESULT_UNCHANGED: &str = "unchanged";
const FETCH_RESULT_NOT_REVOKED: &str = "not_revoked";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Settings {
    pub enabled: bool,
    /// Custom Keybox source URL. Empty means [`DEFAULT_KEYBOX_REMOTE_URL`].
    pub url: String,
    /// Optional proxy template containing [`URL_PLACEHOLDER`], or a prefix
    /// that is concatenated with the Keybox URL. Empty fetches directly.
    pub proxy: String,
    /// Periodic refresh interval in minutes.
    pub interval_minutes: u32,
    /// Replace only when Google's revocation status reports the installed
    /// Keybox as revoked. Enabled by default.
    pub revoked_only: bool,
}

impl Default for Settings {
    fn default() -> Self {
        // The proxy ships preset to the documented mirror template; clearing
        // it in the WebUI selects a direct fetch.
        Self {
            enabled: false,
            url: String::new(),
            proxy: DEFAULT_KEYBOX_REMOTE_PROXY.to_string(),
            interval_minutes: DEFAULT_INTERVAL_MINUTES,
            revoked_only: true,
        }
    }
}

/// Wire payload of `--webui-set-keybox-remote-settings`. The whole payload is
/// base64-encoded JSON so no empty or shell-sensitive argument is ever passed
/// through the KernelSU WebUI bridge. Missing fields fall back to the same
/// defaults as a missing settings file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPayload {
    enabled: bool,
    #[serde(default)]
    url: String,
    #[serde(default = "default_proxy")]
    proxy: String,
    #[serde(default = "default_interval_minutes")]
    interval_minutes: u32,
    #[serde(default = "default_revoked_only")]
    revoked_only: bool,
}

fn default_proxy() -> String {
    DEFAULT_KEYBOX_REMOTE_PROXY.to_string()
}

fn default_interval_minutes() -> u32 {
    DEFAULT_INTERVAL_MINUTES
}

fn default_revoked_only() -> bool {
    true
}

impl SettingsPayload {
    fn into_settings(self) -> Result<Settings> {
        let url = normalize_setting_text(&self.url, "Keybox URL")?;
        let proxy = normalize_setting_text(&self.proxy, "proxy URL")?;
        if !url.is_empty() {
            parse_remote_uri(&url).context("Keybox URL is invalid")?;
        }
        if !proxy.is_empty() {
            parse_remote_uri(&proxy).context("proxy URL is invalid")?;
        }
        if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(&self.interval_minutes) {
            bail!(
                "update interval must be between {MIN_INTERVAL_MINUTES} and {MAX_INTERVAL_MINUTES} minutes"
            );
        }
        Ok(Settings {
            enabled: self.enabled,
            url,
            proxy,
            interval_minutes: self.interval_minutes,
            revoked_only: self.revoked_only,
        })
    }
}

impl Settings {
    /// Builds settings from the base64-decoded JSON wire payload.
    pub fn from_json_payload(payload: &[u8]) -> Result<Self> {
        let payload: SettingsPayload = serde_json::from_slice(payload)
            .context("Keybox auto fetch settings payload is invalid")?;
        payload.into_settings()
    }

    /// Parses the settings file. Legacy three-line files (without the
    /// interval) and four-line files (without the revocation switch) keep
    /// the documented defaults for the missing trailing fields.
    fn parse(contents: &str) -> Result<Self> {
        let lines: Vec<&str> = contents.lines().collect();
        let tokens = match lines.len() {
            3 | 4 => {
                let mut tokens = lines
                    .iter()
                    .map(|line| (*line).to_string())
                    .collect::<Vec<_>>();
                if lines.len() == 3 {
                    tokens.push(DEFAULT_INTERVAL_MINUTES.to_string());
                }
                tokens.push(u8::from(default_revoked_only()).to_string());
                tokens
            }
            5 => lines
                .iter()
                .map(|line| (*line).to_string())
                .collect::<Vec<_>>(),
            count => {
                bail!("Keybox auto fetch settings must contain three to five lines, found {count}")
            }
        };
        Self::from_file_tokens(&tokens)
    }

    fn from_file_tokens(tokens: &[String]) -> Result<Self> {
        let enabled = match tokens[0].as_str() {
            "0" => false,
            "1" => true,
            _ => bail!("Keybox auto fetch enabled must be 0 or 1"),
        };
        let url = normalize_setting_text(&tokens[1], "Keybox URL")?;
        let proxy = normalize_setting_text(&tokens[2], "proxy URL")?;
        if !url.is_empty() {
            parse_remote_uri(&url).context("Keybox URL is invalid")?;
        }
        if !proxy.is_empty() {
            parse_remote_uri(&proxy).context("proxy URL is invalid")?;
        }
        let interval_minutes = match tokens[3].parse::<u32>() {
            Ok(value) => value,
            Err(_) => bail!("Keybox auto fetch interval must be a decimal minute count"),
        };
        if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(&interval_minutes) {
            bail!(
                "update interval must be between {MIN_INTERVAL_MINUTES} and {MAX_INTERVAL_MINUTES} minutes"
            );
        }
        let revoked_only = match tokens[4].as_str() {
            "0" => false,
            "1" => true,
            _ => bail!("Keybox auto fetch revoked-only must be 0 or 1"),
        };
        Ok(Self {
            enabled,
            url,
            proxy,
            interval_minutes,
            revoked_only,
        })
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        format!(
            "{}\n{}\n{}\n{}\n{}\n",
            u8::from(self.enabled),
            self.url,
            self.proxy,
            self.interval_minutes,
            u8::from(self.revoked_only)
        )
        .into_bytes()
    }

    /// Returns the settings, or defaults when the file is missing or
    /// malformed. A damaged file must never turn the feature on.
    fn read_from(path: &Path) -> Self {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(_) => return Self::default(),
        };
        Self::parse(&contents).unwrap_or_default()
    }

    fn persist_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("failed to create OMK data directory")?;
        }
        fs::write(path, self.canonical_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

fn normalize_setting_text(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    if value.len() > MAX_SETTING_BYTES {
        bail!("{label} exceeds the {MAX_SETTING_BYTES} byte limit");
    }
    if value.chars().any(char::is_whitespace) {
        bail!("{label} must not contain whitespace");
    }
    Ok(value.to_string())
}

fn parse_remote_uri(value: &str) -> Result<Uri> {
    let uri: Uri = value
        .parse()
        .map_err(|error| anyhow::anyhow!("URL is invalid: {error}"))?;
    // Plain http stays allowed for user-configured sources; the download is
    // the user's own choice and the payload is validated after transfer.
    if !matches!(uri.scheme_str(), Some("https") | Some("http")) {
        bail!("URL must use http or https");
    }
    let Some(authority) = uri.authority() else {
        bail!("URL has no host");
    };
    if authority.as_str().contains('@') {
        bail!("URL must not contain credentials");
    }
    if uri.host().is_none_or(str::is_empty) {
        bail!("URL has no host");
    }
    Ok(uri)
}

fn is_allowed_remote_keybox_uri(uri: &Uri) -> bool {
    matches!(uri.scheme_str(), Some("https") | Some("http"))
        && uri
            .authority()
            .is_some_and(|authority| !authority.as_str().contains('@'))
        && uri.host().is_some_and(|host| !host.is_empty())
}

/// Substitutes the configured proxy into the effective download URL. A proxy
/// containing `$url` has the placeholder replaced; any other proxy is treated
/// as a prefix, matching the common `https://host/<url>` mirror style.
fn effective_url(settings: &Settings) -> String {
    let url = if settings.url.is_empty() {
        DEFAULT_KEYBOX_REMOTE_URL
    } else {
        settings.url.as_str()
    };
    if settings.proxy.is_empty() {
        return url.to_string();
    }
    if settings.proxy.contains(URL_PLACEHOLDER) {
        settings.proxy.replace(URL_PLACEHOLDER, url)
    } else {
        format!("{}{}", settings.proxy, url)
    }
}

fn decode_keybox_payload(payload: &str) -> Result<Vec<u8>> {
    let compact: String = payload
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    if compact.is_empty() {
        bail!("remote Keybox response is empty");
    }
    // The documented payload is base64-encoded keybox.xml. Fall back to the
    // raw text when it cannot be base64 and already looks like keybox XML so
    // sources serving plain XML keep working.
    match STANDARD.decode(compact.as_bytes()) {
        Ok(decoded) => Ok(decoded),
        Err(_) if is_plain_keybox_xml(&compact) => Ok(payload.trim().as_bytes().to_vec()),
        Err(error) => bail!("remote Keybox payload is not valid base64: {error}"),
    }
}

fn is_plain_keybox_xml(value: &str) -> bool {
    let trimmed = value.trim_start();
    trimmed.starts_with("<?xml") || trimmed.starts_with("<AndroidAttestation")
}

fn fetch_effective_url(url: &str) -> Result<String> {
    let uri: Uri = url
        .parse()
        .map_err(|error| anyhow::anyhow!("effective Keybox URL is invalid: {error}"))?;
    if !is_allowed_remote_keybox_uri(&uri) {
        bail!("effective Keybox URL is not allowed");
    }
    let policy = webui_http::DownloadPolicy {
        resource: "remote Keybox",
        redirect_allowlist: "http(s) endpoints",
        https_only: false,
        max_bytes: MAX_REMOTE_KEYBOX_PAYLOAD_BYTES,
        max_size_label: "256 KiB",
        max_redirects: MAX_REDIRECTS,
        timeout: REMOTE_KEYBOX_TIMEOUT,
        connect_timeout: REMOTE_KEYBOX_CONNECT_TIMEOUT,
    };
    webui_http::download_utf8(uri, &policy, is_allowed_remote_keybox_uri)
}

/// Reports whether Google's attestation status marks the installed Keybox as
/// revoked, using the same status lookup as the WebUI revocation check. Any
/// failure to establish a revocation verdict — a missing or invalid keybox,
/// unreadable certificate serials, or an unreachable status source — counts as
/// "not revoked" so a broken check never silently triggers a replacement.
fn installed_keybox_is_revoked() -> bool {
    let verdict = || -> Result<bool> {
        let (state, _, serials) = keybox::installed_keybox_state_and_metadata()?;
        if matches!(state, keybox::KeyboxFileState::Invalid) {
            return Ok(false);
        }
        let serials = serials.ok_or_else(|| {
            anyhow::anyhow!("installed Keybox certificate serial numbers are unreadable")
        })?;
        Ok(matches!(
            keybox::check_google_attestation_status(&serials)?,
            keybox::KeyboxRevocationStatus::Revoked
        ))
    };
    match verdict() {
        Ok(revoked) => revoked,
        Err(error) => {
            log::warn!(
                "Keybox revocation check failed; treating the installed Keybox as not revoked: {error:#}"
            );
            false
        }
    }
}

/// Downloads the remote Keybox, decodes it, and replaces the installed
/// `keybox.xml` when it differs. Returns `installed`, `unchanged`, or
/// `not_revoked` when the revocation switch skipped the download.
pub fn fetch_and_install() -> Result<&'static str> {
    let settings = Settings::read_from(Path::new(SETTINGS_PATH));
    if settings.revoked_only && !installed_keybox_is_revoked() {
        log::info!("installed Keybox is not revoked; skipping the remote Keybox fetch");
        return Ok(FETCH_RESULT_NOT_REVOKED);
    }
    let url = effective_url(&settings);
    let payload = fetch_effective_url(&url)?;
    let xml = decode_keybox_payload(&payload)?;
    let xml = String::from_utf8(xml).context("decoded remote Keybox is not UTF-8")?;

    let current = fs::read_to_string(KEYBOX_PATH).unwrap_or_default();
    if current.trim() == xml.trim() {
        log::info!("remote Keybox matches the installed keybox.xml; nothing to replace");
        return Ok(FETCH_RESULT_UNCHANGED);
    }

    // Revalidates the payload before the atomic replace; the daemon observes
    // the new file through the existing keybox watcher.
    keybox::install_keybox_xml(xml.as_bytes())?;
    if let Err(error) = webui_activity::record("keybox_remote_replaced", "remote") {
        log::warn!("failed to record the remote Keybox replacement: {error:#}");
    }
    log::info!("installed the Keybox fetched from the remote source");
    Ok(FETCH_RESULT_INSTALLED)
}

pub fn settings_json() -> Result<String> {
    let settings = Settings::read_from(Path::new(SETTINGS_PATH));
    serde_json::to_string(&settings).context("failed to serialize Keybox auto fetch settings")
}

pub fn save_settings(settings: &Settings) -> Result<()> {
    let path = Path::new(SETTINGS_PATH);
    settings.persist_to(path)?;
    // Re-read through the strict parser so a write/read disagreement is
    // surfaced instead of silently enabling or disabling the feature.
    if Settings::read_from(path) != *settings {
        bail!("Keybox auto fetch settings changed while being saved");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(enabled: bool, url: &str, proxy: &str) -> Settings {
        Settings {
            enabled,
            url: url.to_string(),
            proxy: proxy.to_string(),
            interval_minutes: DEFAULT_INTERVAL_MINUTES,
            revoked_only: true,
        }
    }

    fn payload_json(enabled: bool, url: &str, proxy: &str, interval: u32) -> Vec<u8> {
        serde_json::json!({
            "enabled": enabled,
            "url": url,
            "proxy": proxy,
            "interval_minutes": interval,
            "revoked_only": true,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn settings_round_trip_canonical_form() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("keybox_remote.conf");
        let value = Settings {
            interval_minutes: 90,
            revoked_only: false,
            ..settings(
                true,
                "https://example.com/key",
                "https://mirror.example/$url",
            )
        };
        value.persist_to(&path).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "1\nhttps://example.com/key\nhttps://mirror.example/$url\n90\n0\n"
        );
        assert_eq!(Settings::read_from(&path), value);
    }

    #[test]
    fn legacy_three_line_file_keeps_default_interval() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("keybox_remote.conf");
        fs::write(
            &path,
            "1\nhttps://example.com/key\nhttps://mirror.example/$url\n",
        )
        .unwrap();
        let parsed = Settings::read_from(&path);
        assert_eq!(parsed.interval_minutes, DEFAULT_INTERVAL_MINUTES);
        assert_eq!(parsed.url, "https://example.com/key");
        assert!(parsed.revoked_only);
    }

    #[test]
    fn legacy_four_line_file_defaults_to_revoked_only() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("keybox_remote.conf");
        fs::write(
            &path,
            "1\nhttps://example.com/key\nhttps://mirror.example/$url\n90\n",
        )
        .unwrap();
        let parsed = Settings::read_from(&path);
        assert_eq!(parsed.interval_minutes, 90);
        assert!(parsed.revoked_only);
    }

    #[test]
    fn missing_or_damaged_settings_default_to_disabled_with_preset_proxy() {
        let temp_dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Settings::read_from(&temp_dir.path().join("missing")),
            Settings::default()
        );
        let defaults = Settings::default();
        assert!(!defaults.enabled);
        assert_eq!(defaults.proxy, DEFAULT_KEYBOX_REMOTE_PROXY);
        assert_eq!(defaults.url, "");
        assert_eq!(defaults.interval_minutes, DEFAULT_INTERVAL_MINUTES);

        for damaged in [
            "",
            "1\n",
            "1\nurl\n",
            "yes\na\nb\n",
            "1\na\nb\nextra\n99999\n",
            "1\na\nb\nfive\n",
            "1\na\nb\n14\n",
            "1\na\nb\n90\nyes\n",
        ] {
            let path = temp_dir.path().join("damaged.conf");
            fs::write(&path, damaged).unwrap();
            assert_eq!(
                Settings::read_from(&path),
                Settings::default(),
                "{damaged:?}"
            );
        }
    }

    #[test]
    fn json_payload_validates_url_shapes_and_interval() {
        let parsed =
            Settings::from_json_payload(&payload_json(true, "", "https://gh-proxy.org/$url", 360))
                .unwrap();
        assert_eq!(parsed, settings(true, "", "https://gh-proxy.org/$url"));

        // Plain http is a valid user choice for both the source and proxy.
        let plain_http =
            Settings::from_json_payload(&payload_json(true, "http://example.com/key", "", 360))
                .unwrap();
        assert_eq!(plain_http.url, "http://example.com/key");

        for (url, proxy) in [
            ("ftp://example.com/key", ""),
            ("https://u@example.com/key", ""),
            ("https://example.com/ k", ""),
            ("", "ftp://proxy"),
        ] {
            assert!(
                Settings::from_json_payload(&payload_json(true, url, proxy, 360)).is_err(),
                "{url:?} {proxy:?} should be rejected"
            );
        }
        for interval in [MIN_INTERVAL_MINUTES - 1, MAX_INTERVAL_MINUTES + 1] {
            assert!(
                Settings::from_json_payload(&payload_json(true, "", "", interval)).is_err(),
                "interval {interval} should be rejected"
            );
        }
        assert!(Settings::from_json_payload(b"not-json").is_err());
        // Unknown fields are rejected so a stale WebUI cannot silently save
        // settings the helper would misinterpret.
        assert!(Settings::from_json_payload(
            br#"{"enabled":true,"interval_minutes":360,"future":1}"#
        )
        .is_err());
        // Missing url/proxy/interval fields fall back to the documented
        // defaults: built-in source, preset proxy, six-hour interval.
        let minimal = Settings::from_json_payload(br#"{"enabled":true}"#).unwrap();
        assert_eq!(
            minimal,
            Settings {
                enabled: true,
                ..Settings::default()
            }
        );
        // The revocation switch persists in both directions and defaults to
        // enabled when a stale WebUI omits it.
        let explicit = Settings::from_json_payload(
            br#"{"enabled":true,"url":"","proxy":"","interval_minutes":360,"revoked_only":false}"#,
        )
        .unwrap();
        assert!(!explicit.revoked_only);
    }

    #[test]
    fn effective_url_applies_default_and_proxy_modes() {
        // With defaults the preset mirror proxies the default source URL.
        assert_eq!(
            effective_url(&Settings::default()),
            "https://gh-proxy.org/https://raw.githubusercontent.com/Yurii0307/yurikey/main/key"
        );

        // An explicitly cleared proxy fetches directly.
        let direct = effective_url(&settings(true, "", ""));
        assert_eq!(direct, DEFAULT_KEYBOX_REMOTE_URL);

        let custom = effective_url(&settings(true, "https://example.com/key", ""));
        assert_eq!(custom, "https://example.com/key");

        let placeholder = effective_url(&settings(true, "", "https://gh-proxy.org/$url"));
        assert_eq!(
            placeholder,
            "https://gh-proxy.org/https://raw.githubusercontent.com/Yurii0307/yurikey/main/key"
        );

        let prefix = effective_url(&settings(
            true,
            "https://example.com/key",
            "https://mirror.example/",
        ));
        assert_eq!(prefix, "https://mirror.example/https://example.com/key");
    }

    #[test]
    fn composed_proxy_urls_remain_parseable_http_uris() {
        for url in [
            effective_url(&Settings::default()),
            "https://gh-proxy.org/https://raw.githubusercontent.com/Yurii0307/yurikey/main/key"
                .to_string(),
        ] {
            parse_remote_uri(&url).unwrap();
            let uri: Uri = url.parse().unwrap();
            assert!(is_allowed_remote_keybox_uri(&uri));
        }
        // Both schemes pass the allow check; the download client restricts
        // https-only policies per caller.
        for url in ["https://example.com/key", "http://example.com/key"] {
            let uri: Uri = url.parse().unwrap();
            assert!(is_allowed_remote_keybox_uri(&uri), "{url}");
        }
        assert!(!is_allowed_remote_keybox_uri(
            &"ftp://example.com/key".parse().unwrap()
        ));
        assert!(!is_allowed_remote_keybox_uri(
            &"https://user@example.com/key".parse().unwrap()
        ));
    }

    #[test]
    fn decode_accepts_base64_and_plain_xml_payloads() {
        let xml = "<AndroidAttestation>\n</AndroidAttestation>\n";
        let encoded = STANDARD.encode(xml);
        assert_eq!(decode_keybox_payload(&encoded).unwrap(), xml.as_bytes());
        assert_eq!(
            decode_keybox_payload(&format!("{encoded}\r\n")).unwrap(),
            xml.as_bytes()
        );
        assert_eq!(decode_keybox_payload(xml).unwrap(), xml.trim().as_bytes());
        assert!(decode_keybox_payload("").is_err());
        assert!(decode_keybox_payload("!!!!").is_err());
    }

    #[test]
    fn placeholder_constants_match_documented_presets() {
        assert!(DEFAULT_KEYBOX_REMOTE_PROXY.contains(URL_PLACEHOLDER));
        assert_eq!(
            DEFAULT_KEYBOX_REMOTE_URL
                .parse::<Uri>()
                .unwrap()
                .scheme_str(),
            Some("https")
        );
        const {
            assert!(MIN_INTERVAL_MINUTES <= DEFAULT_INTERVAL_MINUTES);
            assert!(DEFAULT_INTERVAL_MINUTES <= MAX_INTERVAL_MINUTES);
        }
    }
}
