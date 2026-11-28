use crate::config::{Config, JResult};
use crate::process;
use jani_core::contract::{
    RELEASE_CHANNEL_SCHEMA as CHANNEL_SCHEMA, RELEASE_COMPONENT,
    RELEASE_MANIFEST_SCHEMA as MANIFEST_SCHEMA, RELEASE_PACKAGE, RELEASE_TARGETS,
    RELEASE_UPDATE_SCHEMA as UPDATE_SCHEMA,
};
use jani_core::crypto;
use ring::digest::{self, Context, SHA256};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::OnceCell;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

mod evidence;
#[cfg(windows)]
mod self_replace;

const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const FETCH_ATTEMPTS: u64 = 4;
const RETRY_BASE: Duration = Duration::from_millis(400);
const RECEIPTS_DIR: &str = "updates";
const RELEASE_SIGNING_DOMAIN: &[u8] = b"jani-release-manifest/v2\0";

pub fn command(cfg: &Config) -> JResult<()> {
    match cfg.sub.as_deref() {
        None | Some("check") => check(cfg),
        Some("install") => install(cfg),
        Some("watch") => watch(cfg),
        #[cfg(windows)]
        Some("recover") => self_replace::recover(cfg),
        Some(other) => Err(format!("unknown update subcommand '{other}'")),
    }
}

fn check(cfg: &Config) -> JResult<()> {
    print_json(&check_report(cfg)?)
}

/// The same report `jani update check` prints, returned for other front ends.
pub fn check_report(cfg: &Config) -> JResult<serde_json::Value> {
    let current_exe = current_exe();
    let source = source(cfg)?;
    let release = fetch_release(&source, cfg)?;
    let latest = KnownRelease::from(&release);
    let store = evidence::Store::new(cfg, &source);
    let _lock = store.read_lock()?;
    let states = inspect_targets(cfg, &latest, current_exe.as_deref(), &store)?;
    let update_available = states.iter().any(|state| state.update_available);
    let report = json!({
        "schemaVersion": UPDATE_SCHEMA,
        "action": "check",
        "source": source,
        "platform": release.platform,
        "target": release.target,
        "tag": release.tag,
        "latestVersion": release.version,
        "commit": release.commit,
        "builtAt": release.built_at,
        "manifestSignatureKey": release.signature_key_id,
        "artifact": artifact_json(&release.artifact),
        "updateAvailable": update_available,
        "targets": states.iter().map(TargetState::json).collect::<Vec<_>>(),
    });
    Ok(report)
}

fn install(cfg: &Config) -> JResult<()> {
    let source = source(cfg)?;
    let outcome = install_once(cfg, &source)?;
    print_json(&outcome.report)?;
    if outcome.failed {
        return Err(outcome
            .failure
            .unwrap_or_else(|| "one or more update targets failed".into()));
    }
    Ok(())
}

fn watch(cfg: &Config) -> JResult<()> {
    let source = source(cfg)?;
    let mut cycle = 0u64;
    loop {
        cycle += 1;
        match install_once(cfg, &source) {
            Ok(outcome) => {
                let mut report = outcome.report;
                if let Some(map) = report.as_object_mut() {
                    map.insert("action".into(), json!("watch"));
                    map.insert("cycle".into(), json!(cycle));
                }
                print_json(&report)?;
                if outcome.recovery_required {
                    return Err(outcome
                        .failure
                        .unwrap_or_else(|| "self-update recovery is required".into()));
                }
                if outcome.restart_required {
                    return Ok(());
                }
            }
            Err(error) => print_json(&json!({
                "schemaVersion": UPDATE_SCHEMA,
                "action": "watch",
                "cycle": cycle,
                "status": "error",
                "error": error,
            }))?,
        }
        if cfg.update_cycles != 0 && cycle >= cfg.update_cycles {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(cfg.update_interval_seconds));
    }
}

fn source(cfg: &Config) -> JResult<String> {
    let source = cfg
        .update_source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(
            "update source is required: pass --source URL or set JANI_UPDATE_SOURCE to the origin serving the releases/ tree",
        )?;
    validate_source(source, cfg.allow_insecure_local_update)
}

fn validate_source(raw: &str, allow_insecure_local: bool) -> JResult<String> {
    let source = raw.trim().trim_end_matches('/');
    if source.is_empty() {
        return Err("update source must not be empty".into());
    }
    if source.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("update source must not contain whitespace or control characters".into());
    }
    if source.contains(['?', '#', '\\']) {
        return Err("update source must not contain a query, fragment or backslash".into());
    }
    let Some((scheme, rest)) = source.split_once("://") else {
        return Err("update source must be an absolute http(s) URL".into());
    };
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "https" | "http") {
        return Err("update source must use https (http is allowed only on loopback)".into());
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        return Err("update source has no host".into());
    }
    if authority.contains('@') {
        return Err("update source must not embed credentials".into());
    }
    if scheme == "http" {
        if !crate::net::is_loopback_authority(authority) {
            return Err("plain-http update sources are allowed only on localhost".into());
        }
        if !allow_insecure_local {
            return Err(
                "plain-http loopback update sources require --allow-insecure-local-update".into(),
            );
        }
    }
    Ok(source.to_string())
}

fn host_platform() -> JResult<(&'static str, &'static str)> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    RELEASE_TARGETS
        .iter()
        .find(|(o, a, _, _)| *o == os && *a == arch)
        .map(|(_, _, platform, target)| (*platform, *target))
        .ok_or_else(|| {
            format!("no release channel mapping for {os}/{arch}; this updater cannot select an artifact")
        })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Channel {
    schema_version: String,
    #[serde(rename = "type")]
    kind: String,
    platform: String,
    latest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestSignature {
    key_id: String,
    value: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: String,
    #[serde(rename = "type")]
    kind: String,
    platform: String,
    package: String,
    version: String,
    tag: String,
    commit: String,
    dirty: bool,
    built_at: String,
    artifacts: Vec<Artifact>,
    signature: Option<ManifestSignature>,
}

impl Manifest {
    fn signing_bytes(&self) -> JResult<Vec<u8>> {
        let mut unsigned = serde_json::to_value(self).map_err(|e| e.to_string())?;
        unsigned
            .as_object_mut()
            .ok_or("release manifest is not an object")?
            .remove("signature");
        let mut bytes = RELEASE_SIGNING_DOMAIN.to_vec();
        bytes.extend(serde_json::to_vec(&unsigned).map_err(|error| error.to_string())?);
        Ok(bytes)
    }

    fn verify(&self, trusted_keys: &[String], revoked_keys: &[String], url: &str) -> JResult<()> {
        let signature = self
            .signature
            .as_ref()
            .ok_or_else(|| format!("release manifest at {url} is unsigned"))?;
        crypto::verify_signature_with_revoked(
            trusted_keys,
            revoked_keys,
            &signature.key_id,
            &self.signing_bytes()?,
            &signature.value,
        )
        .map_err(|error| format!("release manifest signature at {url} is invalid: {error}"))
    }

    fn trusted_artifact(&self, cfg: &Config, tag: &str, context: &str) -> JResult<Artifact> {
        let (platform, target) = host_platform()?;
        validate_manifest(self, platform, target, tag, context)?;
        self.verify(&cfg.update_keys, &cfg.revoked_keys, context)?;
        let mut matching = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.target == target);
        let artifact = matching
            .next()
            .ok_or("release has no artifact for this host")?;
        if matching.next().is_some() {
            return Err("release has multiple artifacts for this host".into());
        }
        validate_artifact(artifact, target)?;
        Ok(artifact.clone())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Artifact {
    target: String,
    binary: String,
    sha256: String,
    size_bytes: u64,
    commit: String,
    dirty: bool,
    built_at: String,
}

struct Release {
    platform: String,
    target: String,
    tag: String,
    version: String,
    commit: String,
    built_at: String,
    signature_key_id: String,
    artifact: Artifact,
    manifest: Arc<Manifest>,
}

struct Origin {
    client: reqwest::blocking::Client,
}

struct FetchError {
    message: String,
    retryable: bool,
}

impl Origin {
    fn new() -> JResult<Self> {
        let client = crate::http::client_builder()
            .timeout(crate::http::REQUEST_TIMEOUT)
            .user_agent(concat!("jani-update/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("cannot build update HTTP client: {e}"))?;
        Ok(Self { client })
    }

    fn get(&self, url: &str, max_bytes: u64) -> JResult<Vec<u8>> {
        let mut failure = String::new();
        for attempt in 1..=FETCH_ATTEMPTS {
            match self.try_get(url, max_bytes) {
                Ok(bytes) => return Ok(bytes),
                Err(error) => {
                    failure = format!("{} after {attempt} attempt(s)", error.message);
                    if !error.retryable || attempt == FETCH_ATTEMPTS {
                        break;
                    }
                    std::thread::sleep(retry_delay(attempt));
                }
            }
        }
        Err(failure)
    }

    fn try_get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, FetchError> {
        let response = self.client.get(url).send().map_err(|error| FetchError {
            message: format!("cannot reach update origin for {url}: {error}"),
            retryable: true,
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(FetchError {
                message: format!("update origin returned HTTP {status} for {url}"),
                retryable: matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes)
        {
            return Err(FetchError {
                message: format!("update response for {url} exceeds {max_bytes} bytes"),
                retryable: false,
            });
        }
        let mut body = Vec::new();
        response
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut body)
            .map_err(|error| FetchError {
                message: format!("cannot read update response from {url}: {error}"),
                retryable: true,
            })?;
        if body.len() as u64 > max_bytes {
            return Err(FetchError {
                message: format!("update response for {url} exceeds {max_bytes} bytes"),
                retryable: false,
            });
        }
        Ok(body)
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str) -> JResult<T> {
        let mut failure = String::new();
        for attempt in 1..=FETCH_ATTEMPTS {
            let bytes = self.get(url, MAX_METADATA_BYTES)?;
            match serde_json::from_slice(&bytes) {
                Ok(value) => return Ok(value),
                Err(error) => {
                    failure = format!("invalid update metadata at {url}: {error}");
                    if attempt == FETCH_ATTEMPTS {
                        break;
                    }
                    std::thread::sleep(retry_delay(attempt));
                }
            }
        }
        Err(failure)
    }
}

fn retry_delay(attempt: u64) -> Duration {
    let base = crate::net::retry_base(crate::env::UPDATE_RETRY_BASE_MS, RETRY_BASE);
    crate::net::retry_delay(base, attempt, crate::net::RETRY_CAP)
}

fn fetch_release(source: &str, cfg: &Config) -> JResult<Release> {
    if cfg.update_keys.is_empty() {
        return Err(
            "update trust root is required: pass --update-key ID:HEX or set JANI_UPDATE_KEYS"
                .into(),
        );
    }
    for key in &cfg.update_keys {
        crypto::parse_key_spec(key)
            .map_err(|error| format!("invalid update trust root: {error}"))?;
    }
    let (platform, target) = host_platform()?;
    let origin = Origin::new()?;
    let channel_url = format!("{source}/{RELEASE_COMPONENT}/{platform}/latest.json");
    let channel: Channel = origin.get_json(&channel_url)?;
    if channel.schema_version != CHANNEL_SCHEMA {
        return Err(format!(
            "unsupported update channel schema '{}' at {channel_url}",
            channel.schema_version
        ));
    }
    if channel.kind != RELEASE_COMPONENT || channel.platform != platform {
        return Err(format!(
            "update channel at {channel_url} is for {} {}; expected {RELEASE_COMPONENT} {platform}",
            channel.kind, channel.platform
        ));
    }
    if !safe_component(&channel.latest) {
        return Err("update channel latest version is not a safe path component".into());
    }
    let manifest_url = format!(
        "{source}/{RELEASE_COMPONENT}/{platform}/{}/manifest.json",
        channel.latest
    );
    let manifest: Manifest = origin.get_json(&manifest_url)?;
    let artifact = manifest.trusted_artifact(cfg, &channel.latest, &manifest_url)?;
    Ok(Release {
        platform: platform.to_string(),
        target: target.to_string(),
        tag: manifest.tag.clone(),
        version: manifest.version.clone(),
        commit: manifest.commit.clone(),
        built_at: manifest.built_at.clone(),
        signature_key_id: manifest
            .signature
            .as_ref()
            .map(|signature| signature.key_id.clone())
            .unwrap_or_default(),
        artifact,
        manifest: Arc::new(manifest),
    })
}

fn validate_manifest(
    manifest: &Manifest,
    platform: &str,
    target: &str,
    latest: &str,
    url: &str,
) -> JResult<()> {
    if manifest.schema_version != MANIFEST_SCHEMA {
        return Err(format!(
            "unsupported release manifest schema '{}' at {url}",
            manifest.schema_version
        ));
    }
    if manifest.kind != RELEASE_COMPONENT || manifest.platform != platform {
        return Err(format!(
            "release manifest at {url} is for {} {}; expected {RELEASE_COMPONENT} {platform}",
            manifest.kind, manifest.platform
        ));
    }
    if manifest.package != RELEASE_PACKAGE {
        return Err(format!(
            "release manifest at {url} packages '{}'; expected {RELEASE_PACKAGE}",
            manifest.package
        ));
    }
    if Version::parse(&manifest.version).is_none() {
        return Err(format!(
            "release manifest version '{}' is invalid",
            manifest.version
        ));
    }
    if latest != manifest.tag
        || (manifest.tag != manifest.version && manifest.tag != format!("v{}", manifest.version))
    {
        return Err(format!(
            "release tag '{}' does not match channel '{latest}' and version '{}'",
            manifest.tag, manifest.version
        ));
    }
    if !safe_component(&manifest.tag) {
        return Err("release tag is not a safe path component".into());
    }
    if manifest.dirty {
        return Err("release manifest describes a dirty build; refusing to distribute it".into());
    }
    if !valid_commit(&manifest.commit) || !valid_built_at(&manifest.built_at) {
        return Err("release manifest is missing a valid commit or build time".into());
    }
    if manifest.artifacts.is_empty() {
        return Err("release manifest has no artifacts".into());
    }
    if !manifest
        .artifacts
        .iter()
        .any(|artifact| artifact.target == target)
    {
        return Err(format!("release manifest has no artifact for {target}"));
    }
    Ok(())
}

fn validate_artifact(artifact: &Artifact, target: &str) -> JResult<()> {
    if artifact.target != target {
        return Err(format!(
            "artifact target '{}' does not match host {target}",
            artifact.target
        ));
    }
    if !safe_file_name(&artifact.binary) {
        return Err(format!(
            "artifact binary name '{}' is not a plain file name",
            artifact.binary
        ));
    }
    if artifact.size_bytes == 0 || artifact.size_bytes > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "artifact size {} is outside 1..{MAX_ARTIFACT_BYTES} bytes",
            artifact.size_bytes
        ));
    }
    if artifact.sha256.len() != 64 || !artifact.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("artifact sha256 is not a 64-character hex digest".into());
    }
    if artifact.dirty {
        return Err("artifact was built from a dirty tree; refusing to distribute it".into());
    }
    if !valid_commit(&artifact.commit) || !valid_built_at(&artifact.built_at) {
        return Err("artifact is missing a valid commit or build time".into());
    }
    Ok(())
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

fn safe_file_name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && !value.contains(['/', '\\', ':'])
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

fn valid_commit(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_built_at(value: &str) -> bool {
    (10..=40).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ':' | '+' | '.'))
}

#[derive(Debug, Eq, PartialEq)]
struct Version {
    numbers: Vec<u64>,
    pre: Option<String>,
}

impl Version {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.strip_prefix('v').unwrap_or(raw);
        if raw.is_empty()
            || raw
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')))
        {
            return None;
        }
        let without_build = raw.split_once('+').map(|(left, _)| left).unwrap_or(raw);
        let (core, pre) = match without_build.split_once('-') {
            Some((core, pre)) if !pre.is_empty() => (core, Some(pre.to_string())),
            Some(_) => return None,
            None => (without_build, None),
        };
        let mut numbers = Vec::new();
        for part in core.split('.') {
            if part.is_empty() {
                return None;
            }
            numbers.push(part.parse::<u64>().ok()?);
        }
        if numbers.is_empty() || numbers.len() > 4 {
            return None;
        }
        Some(Self { numbers, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let width = self.numbers.len().max(other.numbers.len());
        for index in 0..width {
            let left = self.numbers.get(index).copied().unwrap_or(0);
            let right = other.numbers.get(index).copied().unwrap_or(0);
            match left.cmp(&right) {
                Ordering::Equal => {}
                ordering => return ordering,
            }
        }
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => compare_prerelease(left, right),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_prerelease(left: &str, right: &str) -> Ordering {
    let left = left.split('.').collect::<Vec<_>>();
    let right = right.split('.').collect::<Vec<_>>();
    let width = left.len().max(right.len());
    for index in 0..width {
        let Some(a) = left.get(index) else {
            return Ordering::Less;
        };
        let Some(b) = right.get(index) else {
            return Ordering::Greater;
        };
        let ordering = match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => a.cmp(b),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn compare_versions(latest: &str, current: &str) -> Option<Ordering> {
    Some(Version::parse(latest)?.cmp(&Version::parse(current)?))
}

struct TargetState {
    path: PathBuf,
    current: Option<String>,
    error: Option<String>,
    update_available: bool,
    status: &'static str,
    restart_required: bool,
    recovery_required: bool,
    matched_latest: bool,
    retained: Option<Arc<Manifest>>,
}

impl TargetState {
    fn json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "currentVersion": self.current,
            "status": self.status,
            "updateAvailable": self.update_available,
            "restartRequired": self.restart_required,
            "recoveryRequired": self.recovery_required,
            "error": self.error,
        })
    }
}

fn current_exe() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok())
}

fn resolve_targets(cfg: &Config) -> JResult<Vec<PathBuf>> {
    let mut true_targets = Vec::new();
    if cfg.update_targets.is_empty() {
        let path = std::env::current_exe()
            .map_err(|e| format!("cannot locate the running jani executable: {e}"))?;
        true_targets.push(path);
    } else {
        for raw in &cfg.update_targets {
            let path = if raw.is_absolute() {
                raw.clone()
            } else {
                std::env::current_dir()
                    .map_err(|e| format!("cannot resolve --to {}: {e}", raw.display()))?
                    .join(raw)
            };
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("--to {} has no usable file name", raw.display()))?;
            if !safe_file_name(name) {
                return Err(format!("--to {} has an unsafe file name", raw.display()));
            }
            if !path.parent().is_some_and(Path::is_dir) {
                return Err(format!(
                    "--to {} needs an existing parent directory",
                    raw.display()
                ));
            }
            true_targets.push(path);
        }
    }
    let mut unique = Vec::new();
    let mut resolved = BTreeSet::new();
    for target in true_targets {
        #[cfg(windows)]
        if self_replace::reserved(&target) {
            return Err("self-update backup and lock paths are reserved. Use update recover with the original target path".into());
        }
        let identity = evidence::canonical_target(&target).unwrap_or_else(|_| target.clone());
        if resolved.insert(identity) {
            unique.push(target);
        }
    }
    Ok(unique)
}

struct CurrentExecutable<'a> {
    path: &'a Path,
    size: Option<u64>,
    digest: OnceCell<Option<String>>,
}

impl<'a> CurrentExecutable<'a> {
    fn new(path: &'a Path) -> Self {
        Self {
            path,
            size: fs::symlink_metadata(path)
                .ok()
                .filter(|metadata| {
                    evidence::regular(metadata) && metadata.len() <= MAX_ARTIFACT_BYTES
                })
                .map(|metadata| metadata.len()),
            digest: OnceCell::new(),
        }
    }

    fn matches(&self, size: u64, digest: &str) -> bool {
        self.size == Some(size)
            && self
                .digest
                .get_or_init(|| {
                    let file = open_sized_target(self.path, size).ok()?;
                    file_digest(&file, size).ok()
                })
                .as_deref()
                == Some(digest)
    }
}

#[derive(Clone)]
struct KnownRelease {
    size: u64,
    digest: String,
    version: String,
}

impl From<&Release> for KnownRelease {
    fn from(release: &Release) -> Self {
        Self::new(&release.version, &release.artifact)
    }
}

impl KnownRelease {
    fn new(version: &str, artifact: &Artifact) -> Self {
        Self {
            size: artifact.size_bytes,
            digest: artifact.sha256.clone(),
            version: version.into(),
        }
    }

    fn matches(&self, size: u64, digest: &str) -> bool {
        self.size == size && self.digest.eq_ignore_ascii_case(digest)
    }
}

fn inspect_targets(
    cfg: &Config,
    latest: &KnownRelease,
    current_exe: Option<&Path>,
    store: &evidence::Store<'_>,
) -> JResult<Vec<TargetState>> {
    let current = current_exe.map(CurrentExecutable::new);
    Ok(resolve_targets(cfg)?
        .into_iter()
        .map(|path| inspect_target(path, latest, current.as_ref(), store, cfg.update_force))
        .collect())
}

fn open_sized_target(path: &Path, size: u64) -> JResult<fs::File> {
    if size > MAX_ARTIFACT_BYTES {
        return Err("target exceeds the supported release size".into());
    }
    let file = evidence::file_options()
        .read(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !evidence::regular(&metadata) {
        return Err("target is not a regular file".into());
    }
    if metadata.len() != size {
        return Err("target size changed before inspection".into());
    }
    Ok(file)
}

fn file_digest(input: impl Read, size: u64) -> JResult<String> {
    let mut file = input.take(size + 1);
    let mut hasher = Context::new(&SHA256);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if file.limit() != 1 {
        return Err("target size changed during inspection".into());
    }
    Ok(hex::encode(hasher.finish()))
}

#[derive(Debug, PartialEq, Eq)]
enum VersionMatch {
    Unknown,
    Running,
    Retained(usize),
    Latest,
}

fn inspected_version(
    path: &Path,
    size: u64,
    latest: &KnownRelease,
    running: Option<&CurrentExecutable<'_>>,
    previous: [Option<&KnownRelease>; 2],
) -> JResult<VersionMatch> {
    // Opening even an unrecognized target preserves unreadable-file errors.
    let file = open_sized_target(path, size)?;
    if size != latest.size
        && previous.iter().flatten().all(|known| size != known.size)
        && running.and_then(|exe| exe.size) != Some(size)
    {
        return Ok(VersionMatch::Unknown);
    }
    let digest = file_digest(&file, size)?;
    // Incumbent evidence wins over a relabel of the same bytes. A running
    // executable's path can refer to a replacement after a self-update.
    if let Some(index) = previous
        .iter()
        .position(|known| known.is_some_and(|known| known.matches(size, &digest)))
    {
        Ok(VersionMatch::Retained(index))
    } else if running.is_some_and(|exe| exe.matches(size, &digest)) {
        if latest.version == env!("CARGO_PKG_VERSION") && latest.matches(size, &digest) {
            Ok(VersionMatch::Latest)
        } else {
            Ok(VersionMatch::Running)
        }
    } else if latest.matches(size, &digest) {
        Ok(VersionMatch::Latest)
    } else {
        Ok(VersionMatch::Unknown)
    }
}

fn inspect_target(
    path: PathBuf,
    latest: &KnownRelease,
    running: Option<&CurrentExecutable<'_>>,
    store: &evidence::Store<'_>,
    force: bool,
) -> TargetState {
    let restart_required = running
        .and_then(|exe| fs::canonicalize(&path).ok().map(|path| path == exe.path))
        .unwrap_or(false);
    let unavailable = |error: String| TargetState {
        path: path.clone(),
        current: None,
        error: Some(error),
        update_available: false,
        status: "error",
        restart_required,
        recovery_required: false,
        matched_latest: false,
        retained: None,
    };
    let file_size = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !evidence::regular(&metadata) {
                return unavailable("target exists but is not a regular file".into());
            }
            Some(metadata.len())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return unavailable(error.to_string()),
    };
    #[cfg(windows)]
    {
        match self_replace::recovery_required(store, &path, file_size.is_some()) {
            Ok(Some(error)) | Err(error) => {
                let mut state = unavailable(error);
                state.recovery_required = true;
                return state;
            }
            Ok(None) => (),
        }
    }
    let (current, matched_latest, retained) = if let Some(size) = file_size {
        if restart_required {
            (Some(env!("CARGO_PKG_VERSION").to_owned()), false, None)
        } else {
            let mut proofs = match store.read(&path) {
                Ok(proofs) => proofs,
                Err(error) => return unavailable(error),
            };
            let previous = proofs
                .each_ref()
                .map(|proof| proof.as_ref().map(|proof| &proof.known));
            match inspected_version(&path, size, latest, running, previous) {
                Ok(VersionMatch::Unknown) => (None, false, None),
                Ok(VersionMatch::Running) => {
                    (Some(env!("CARGO_PKG_VERSION").to_owned()), false, None)
                }
                Ok(VersionMatch::Latest) => (Some(latest.version.clone()), true, None),
                Ok(VersionMatch::Retained(index)) => {
                    let proof = proofs[index]
                        .take()
                        .expect("matched a verified release proof");
                    (Some(proof.known.version), false, Some(proof.manifest))
                }
                Err(error) => return unavailable(error),
            }
        }
    } else {
        (None, false, None)
    };
    let exists = file_size.is_some();
    let comparison = current
        .as_deref()
        .and_then(|current| compare_versions(&latest.version, current));
    let update_available = force || !exists || comparison == Some(Ordering::Greater);
    let status = if update_available {
        "update-available"
    } else if exists && current.is_none() {
        "unknown-version"
    } else if comparison == Some(Ordering::Less) {
        "newer-local"
    } else if current.is_some() {
        "up-to-date"
    } else {
        "update-available"
    };
    TargetState {
        path,
        current,
        error: None,
        update_available,
        status,
        restart_required,
        recovery_required: false,
        matched_latest,
        retained,
    }
}

fn binary_version(path: &Path, credential_env_names: &[String]) -> JResult<String> {
    let cwd = path
        .parent()
        .filter(|parent| parent.is_dir())
        .unwrap_or_else(|| Path::new("."));
    let mut command = Command::new(path);
    command.arg("--version");
    let output = process::execute_with_scrubbed(
        &mut command,
        cwd,
        10,
        crate::process::ProcessIsolation::Direct,
        credential_env_names,
    )
    .map_err(|e| format!("cannot run {} --version: {e}", path.display()))?;
    if output.code != Some(0) {
        return Err(format!(
            "{} --version exited with {:?}; refusing to install an unrecognized binary",
            path.display(),
            output.code
        ));
    }
    let line = output
        .stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or_default();
    let version = line
        .strip_prefix("jani ")
        .ok_or_else(|| format!("{} --version did not identify a jani build", path.display()))?;
    if Version::parse(version).is_none() {
        return Err(format!(
            "{} reports an invalid version '{version}'",
            path.display()
        ));
    }
    Ok(version.to_string())
}

struct InstallOutcome {
    report: Value,
    failed: bool,
    failure: Option<String>,
    restart_required: bool,
    recovery_required: bool,
}

fn install_once(cfg: &Config, source: &str) -> JResult<InstallOutcome> {
    let release = fetch_release(source, cfg)?;
    let store = evidence::Store::new(cfg, source);
    let _lock = store.lock()?;
    let current_exe = current_exe();
    let latest = KnownRelease::from(&release);
    let mut states = inspect_targets(cfg, &latest, current_exe.as_deref(), &store)?;
    let mut recovery_required = states.iter().any(|state| state.recovery_required);
    for state in &mut states {
        if state.matched_latest && !recovery_required {
            // Publication precedes any network download or target replacement.
            if let Err(error) = store.write(&state.path, Some(&release.manifest), None) {
                state.error = Some(error);
                state.status = "error";
                state.update_available = false;
            } else {
                state.retained = Some(Arc::clone(&release.manifest));
            }
        }
    }
    let needs_install = !recovery_required
        && states
            .iter()
            .any(|state| state.update_available && state.error.is_none());
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut installed = 0usize;
    let mut artifact_verified = false;
    let mut restart_required = false;

    if needs_install {
        let url = format!(
            "{source}/{RELEASE_COMPONENT}/{}/{}/{}",
            release.platform, release.tag, release.artifact.binary
        );
        let bytes = Origin::new()?.get(&url, release.artifact.size_bytes)?;
        if bytes.len() as u64 != release.artifact.size_bytes {
            return Err(format!(
                "downloaded {} bytes from {url}; manifest declares {}",
                bytes.len(),
                release.artifact.size_bytes
            ));
        }
        let digest = hex::encode(digest::digest(&SHA256, &bytes));
        if !digest.eq_ignore_ascii_case(&release.artifact.sha256) {
            return Err(format!(
                "downloaded artifact hash {digest} does not match manifest {}",
                release.artifact.sha256
            ));
        }
        artifact_verified = true;

        let mut applied = BTreeSet::new();
        for state in &states {
            if let Some(error) = &state.error {
                failures.push(format!("{}: {error}", state.path.display()));
                results.push(state.json());
                continue;
            }
            if !state.update_available || recovery_required {
                results.push(state.json());
                continue;
            }
            // Two initially absent spellings can resolve to the same file on
            // Windows after the first path has been installed.
            let identity =
                evidence::canonical_target(&state.path).unwrap_or_else(|_| state.path.clone());
            if applied.contains(&identity) {
                continue;
            }
            let mut needs_recovery = false;
            match install_target(
                state,
                &bytes,
                &release,
                &cfg.credential_env_names,
                &store,
                &mut needs_recovery,
            ) {
                Ok(result) => {
                    installed += 1;
                    restart_required |= state.restart_required;
                    applied.insert(evidence::canonical_target(&state.path).unwrap_or(identity));
                    results.push(result);
                }
                Err(error) => {
                    recovery_required |= needs_recovery;
                    failures.push(format!("{}: {error}", state.path.display()));
                    results.push(json!({
                        "path": state.path.display().to_string(),
                        "currentVersion": state.current,
                        "status": "error",
                        "updateAvailable": true,
                        "restartRequired": state.restart_required,
                        "recoveryRequired": needs_recovery,
                        "error": error,
                    }));
                }
            }
        }
    } else {
        for state in &states {
            if let Some(error) = &state.error {
                failures.push(format!("{}: {error}", state.path.display()));
            }
            results.push(state.json());
        }
    }

    let status = if recovery_required {
        "recovery-required"
    } else if !failures.is_empty() {
        if installed > 0 {
            "partial"
        } else {
            "error"
        }
    } else if installed > 0 {
        "installed"
    } else if states.iter().any(|state| state.status == "unknown-version") {
        "unknown-version"
    } else if states.iter().any(|state| state.status == "newer-local") {
        "newer-local"
    } else {
        "up-to-date"
    };
    let mut report = json!({
        "schemaVersion": UPDATE_SCHEMA,
        "action": "install",
        "source": source,
        "platform": release.platform,
        "target": release.target,
        "tag": release.tag,
        "latestVersion": release.version,
        "commit": release.commit,
        "builtAt": release.built_at,
        "manifestSignatureKey": release.signature_key_id,
        "artifact": artifact_json(&release.artifact),
        "artifactVerified": artifact_verified,
        "status": status,
        "recoveryRequired": recovery_required,
        "targets": results,
    });
    if let Some((path, error)) = write_receipt(cfg, &report) {
        if let Some(map) = report.as_object_mut() {
            map.insert(
                "receipt".into(),
                path.map(|path| json!(path)).unwrap_or_else(|| Value::Null),
            );
            if let Some(error) = error {
                map.insert("receiptError".into(), json!(error));
            }
        }
    }
    Ok(InstallOutcome {
        report,
        failed: !failures.is_empty(),
        restart_required,
        recovery_required,
        failure: if failures.is_empty() {
            None
        } else {
            Some(failures.join("; "))
        },
    })
}

fn install_target(
    state: &TargetState,
    bytes: &[u8],
    release: &Release,
    credential_env_names: &[String],
    store: &evidence::Store<'_>,
    recovery_required: &mut bool,
) -> JResult<Value> {
    let path = &state.path;
    let destination = if state.restart_required {
        path.clone()
    } else {
        evidence::canonical_target(path)?
    };
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("target has no usable file name")?;
    let parent = destination
        .parent()
        .ok_or("target has no parent directory")?;
    let candidate = store.staging_path(parent, name)?;
    evidence::clear_staging(&candidate)?;
    write_candidate(&candidate, bytes)?;
    let verified = (|| -> JResult<()> {
        let mut last = String::new();
        for attempt in 1..=3 {
            match binary_version(&candidate, credential_env_names) {
                Ok(version) if version == release.version => return Ok(()),
                Ok(version) => {
                    last = format!(
                        "downloaded binary reports {version}; manifest declares {}",
                        release.version
                    );
                }
                Err(error) => last = error,
            }
            if attempt < 3 {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        Err(last)
    })();
    if let Err(error) = verified {
        let _ = fs::remove_file(&candidate);
        return Err(format!("downloaded update failed verification: {error}"));
    }
    let publish = (|| {
        let size = release.artifact.size_bytes;
        let staged = open_sized_target(&candidate, size)?;
        if !file_digest(&staged, size)?.eq_ignore_ascii_case(&release.artifact.sha256) {
            return Err("staged update bytes changed during verification".into());
        }
        drop(staged);
        #[cfg(windows)]
        if state.restart_required {
            return self_replace::replace(store, path, &candidate, release, recovery_required);
        }
        if !state.restart_required {
            store.write(
                &destination,
                state.retained.as_deref(),
                Some(&release.manifest),
            )?;
        }
        fs::rename(&candidate, &destination)
            .map_err(|error| format!("cannot install {} ({error})", destination.display()))?;
        Ok(None)
    })();
    let backup = match publish {
        Ok(backup) => backup,
        Err(error) => {
            if !*recovery_required {
                let _ = fs::remove_file(&candidate);
            }
            return Err(error);
        }
    };
    Ok(json!({
        "path": path.display().to_string(),
        "previousVersion": state.current,
        "newVersion": release.version,
        "status": "installed",
        "updateAvailable": false,
        "restartRequired": state.restart_required,
        "sha256": release.artifact.sha256,
        "bytes": bytes.len(),
        "backup": backup.map(|path: PathBuf| path.display().to_string()),
    }))
}

fn write_candidate(path: &Path, bytes: &[u8]) -> JResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o700);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let written = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o755))?;
        }
        file.sync_all()
    })();
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(path);
        return Err(format!("cannot stage {}: {error}", path.display()));
    }
    Ok(())
}

fn artifact_json(artifact: &Artifact) -> Value {
    json!({
        "binary": artifact.binary,
        "target": artifact.target,
        "sha256": artifact.sha256,
        "sizeBytes": artifact.size_bytes,
        "commit": artifact.commit,
        "builtAt": artifact.built_at,
    })
}

fn write_receipt(cfg: &Config, report: &Value) -> Option<(Option<String>, Option<String>)> {
    let dir = cfg.state_dir.join(RECEIPTS_DIR);
    if let Err(error) = fs::create_dir_all(&dir) {
        return Some((
            None,
            Some(format!(
                "cannot create update receipt directory {}: {error}",
                dir.display()
            )),
        ));
    }
    let path = dir.join(format!("{}.json", process::unique_id()));
    let body = serde_json::to_vec_pretty(report).ok()?;
    match fs::write(&path, body) {
        Ok(()) => Some((Some(path.display().to_string()), None)),
        Err(error) => Some((
            None,
            Some(format!(
                "cannot write update receipt {}: {error}",
                path.display()
            )),
        )),
    }
}

fn print_json(value: &Value) -> JResult<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_fingerprints_count_bytes_and_stop_at_the_declared_size_plus_one() {
        let bytes = b"abcd";
        assert_eq!(
            file_digest(&bytes[..], 4).unwrap(),
            crate::types::sha256_hex(bytes)
        );
        assert!(file_digest(&bytes[..], 5)
            .unwrap_err()
            .contains("size changed"));
        let mut input = std::io::Cursor::new(b"abcdefgh");
        assert!(file_digest(&mut input, 4)
            .unwrap_err()
            .contains("size changed"));
        assert_eq!(input.position(), 5);
    }

    #[test]
    fn running_digest_is_lazy_and_memoizes_success_and_failure_for_one_poll() {
        let root = std::env::temp_dir().join(process::unique_id());
        fs::create_dir(&root).unwrap();
        let path = root.join("running");
        fs::write(&path, b"build").unwrap();
        let running = CurrentExecutable::new(&path);
        let expected = crate::types::sha256_hex(b"build");
        assert!(!running.matches(4, &expected));
        assert!(running.digest.get().is_none());
        assert!(running.matches(5, &expected));
        let second_poll = CurrentExecutable::new(&path);
        fs::remove_file(&path).unwrap();
        assert!(running.matches(5, &expected));
        assert!(!second_poll.matches(5, &expected));
        assert_eq!(second_poll.digest.get(), Some(&None));
        fs::write(&path, b"build").unwrap();
        assert!(!second_poll.matches(5, &expected));
        assert!(CurrentExecutable::new(&path).matches(5, &expected));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remembered_release_requires_current_bytes_and_refuses_relabeling() {
        let root = std::env::temp_dir().join(process::unique_id());
        fs::create_dir(&root).unwrap();
        let target = root.join("standalone");
        fs::write(&target, b"build").unwrap();
        let prior = KnownRelease {
            size: 5,
            digest: crate::types::sha256_hex(b"build"),
            version: "9.9.2".into(),
        };
        let latest = KnownRelease {
            version: "9.9.1".into(),
            ..prior.clone()
        };
        assert_eq!(
            inspected_version(&target, 5, &latest, None, [Some(&prior), None]).unwrap(),
            VersionMatch::Retained(0)
        );
        let higher = KnownRelease {
            version: "9.9.3".into(),
            ..prior.clone()
        };
        assert_eq!(
            inspected_version(&target, 5, &higher, None, [Some(&prior), None]).unwrap(),
            VersionMatch::Retained(0)
        );
        fs::write(&target, b"other").unwrap();
        assert_eq!(
            inspected_version(&target, 5, &latest, None, [Some(&prior), None]).unwrap(),
            VersionMatch::Unknown
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remembered_release_does_not_transfer_to_an_identical_target_at_another_path() {
        let root = std::env::temp_dir().join(process::unique_id());
        fs::create_dir(&root).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        fs::write(&first, b"build").unwrap();
        fs::copy(&first, &second).unwrap();
        let previous = signed_fixture("9.9.2", b"build", "incumbent", &"11".repeat(32));
        let latest = KnownRelease {
            size: 5,
            digest: crate::types::sha256_hex(b"other"),
            version: "9.9.3".into(),
        };
        let mut cfg = Config::baseline();
        cfg.state_dir = root.join("state");
        cfg.update_keys = vec![fixture_key("incumbent", &"11".repeat(32))];
        cfg.update_targets = vec![first.clone(), second];
        let store = evidence::Store::new(&cfg, "https://releases.example");
        let lock = store.lock().unwrap();
        store.write(&first, Some(&previous), None).unwrap();
        assert!(evidence::Store::new(&cfg, "https://other.example")
            .read(&first)
            .unwrap()
            .iter()
            .all(Option::is_none));
        let states = inspect_targets(&cfg, &latest, None, &store).unwrap();
        assert_eq!(states[0].current.as_deref(), Some("9.9.2"));
        assert!(states[0].update_available);
        assert_eq!(states[1].status, "unknown-version");
        assert!(!states[1].update_available);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_proofs_recognize_both_sides_of_replacement_and_apply_current_revocation() {
        let root = std::env::temp_dir().join(process::unique_id());
        fs::create_dir(&root).unwrap();
        let target = root.join("standalone");
        fs::write(&target, b"before").unwrap();
        let incumbent = signed_fixture("9.9.1", b"before", "old", &"11".repeat(32));
        let candidate = signed_fixture("9.9.2", b"after!", "new", &"22".repeat(32));
        let newer = signed_fixture("9.9.3", b"newest", "new", &"22".repeat(32));
        let latest = KnownRelease::new(&newer.version, &newer.artifacts[0]);
        let mut cfg = Config::baseline();
        cfg.state_dir = root.join("state");
        cfg.update_keys = vec![
            fixture_key("old", &"11".repeat(32)),
            fixture_key("new", &"22".repeat(32)),
        ];
        cfg.update_targets = vec![target.clone()];
        let source = "https://releases.example";
        {
            let store = evidence::Store::new(&cfg, source);
            let _lock = store.lock().unwrap();
            assert!(store.lock().unwrap_err().contains("another updater"));
            assert!(store.read_lock().unwrap_err().contains("another updater"));
            let pending = cfg.state_dir.join("update-proofs").join(".pending");
            fs::write(&pending, b"abandoned write fixture").unwrap();
            store
                .write(&target, Some(&incumbent), Some(&candidate))
                .unwrap();
            assert!(!pending.exists());
        }
        {
            let store = evidence::Store::new(&cfg, source);
            let shared = store.read_lock().unwrap().unwrap();
            assert!(store.lock().unwrap_err().contains("another updater"));
            drop(shared);
        }
        let current = |cfg: &Config| {
            inspect_targets(cfg, &latest, None, &evidence::Store::new(cfg, source))
                .unwrap()
                .remove(0)
                .current
        };
        assert_eq!(current(&cfg).as_deref(), Some("9.9.1"));
        cfg.revoked_keys = vec!["old".into()];
        assert_eq!(current(&cfg), None);
        let staged = root.join("candidate");
        fs::write(&staged, b"after!").unwrap();
        fs::rename(&staged, &target).unwrap();
        assert_eq!(current(&cfg).as_deref(), Some("9.9.2"));
        fs::write(&target, b"oops!!").unwrap();
        assert_eq!(current(&cfg), None);
        fs::remove_dir_all(root).unwrap();
    }

    fn fixture_key(id: &str, secret: &str) -> String {
        format!("{id}:{}", crypto::public_key_hex(secret).unwrap())
    }

    #[test]
    fn signed_manifest_cannot_select_between_multiple_host_artifacts() {
        let secret = "11".repeat(32);
        let mut sample = signed_fixture("9.9.1", b"first", "release", &secret);
        let mut other = sample.artifacts[0].clone();
        other.sha256 = crate::types::sha256_hex(b"other");
        sample.artifacts.push(other);
        sample.signature.as_mut().unwrap().value =
            crypto::sign_hex(&secret, &sample.signing_bytes().unwrap()).unwrap();
        let mut cfg = Config::baseline();
        cfg.update_keys = vec![fixture_key("release", &secret)];
        assert!(sample
            .trusted_artifact(&cfg, &sample.tag, "test release")
            .unwrap_err()
            .contains("multiple artifacts"));
    }

    fn signed_fixture(version: &str, bytes: &[u8], id: &str, secret: &str) -> Manifest {
        let mut sample = manifest();
        let (platform, target) = host_platform().unwrap();
        sample.platform = platform.into();
        sample.version = version.into();
        sample.tag = format!("v{version}");
        sample.artifacts[0].target = target.into();
        sample.artifacts[0].sha256 = crate::types::sha256_hex(bytes);
        sample.artifacts[0].size_bytes = bytes.len() as u64;
        sample.signature = Some(ManifestSignature {
            key_id: id.into(),
            value: crypto::sign_hex(secret, &sample.signing_bytes().unwrap()).unwrap(),
        });
        sample
    }

    fn manifest() -> Manifest {
        Manifest {
            schema_version: MANIFEST_SCHEMA.into(),
            kind: RELEASE_COMPONENT.into(),
            platform: "windows".into(),
            package: RELEASE_PACKAGE.into(),
            version: "0.2.0".into(),
            tag: "v0.2.0".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            dirty: false,
            built_at: "2026-10-26T00:00:00Z".into(),
            artifacts: vec![Artifact {
                target: "x86_64-pc-windows-msvc".into(),
                binary: "jani-0.2.0-x86_64-pc-windows-msvc.exe".into(),
                sha256: "a".repeat(64),
                size_bytes: 1024,
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                dirty: false,
                built_at: "2026-10-26T00:00:00Z".into(),
            }],
            signature: None,
        }
    }

    #[test]
    fn release_manifest_signature_is_domain_separated_and_tamper_evident() {
        let secret = "55".repeat(32);
        let public = crypto::public_key_hex(&secret).unwrap();
        let mut sample = manifest();
        let bytes = sample.signing_bytes().unwrap();
        assert!(bytes.starts_with(RELEASE_SIGNING_DOMAIN));
        sample.signature = Some(ManifestSignature {
            key_id: "release-test".into(),
            value: crypto::sign_hex(&secret, &bytes).unwrap(),
        });
        sample
            .verify(
                &[format!("release-test:{public}")],
                &[],
                "https://releases.example",
            )
            .unwrap();

        let mut tampered = sample;
        tampered.version = "0.2.1".into();
        assert!(tampered
            .verify(
                &[format!("release-test:{public}")],
                &[],
                "https://releases.example"
            )
            .is_err());
        let wrong_key = "66".repeat(32);
        let wrong_public = crypto::public_key_hex(&wrong_key).unwrap();
        let mut sample = manifest();
        sample.signature = Some(ManifestSignature {
            key_id: "release-test".into(),
            value: crypto::sign_hex(&wrong_key, &sample.signing_bytes().unwrap()).unwrap(),
        });
        assert!(sample
            .verify(
                &[format!("release-test:{public}")],
                &[],
                "https://releases.example"
            )
            .is_err());
        sample
            .verify(
                &[format!("release-test:{wrong_public}")],
                &[],
                "https://releases.example",
            )
            .unwrap();
    }

    #[test]
    fn release_manifest_must_be_signed_by_an_explicit_trusted_key() {
        let sample = manifest();
        let err = sample
            .verify(&["11".repeat(32)], &[], "https://releases.example")
            .unwrap_err();
        assert!(err.contains("unsigned"));
    }

    #[test]
    fn versions_compare_semantically() {
        assert_eq!(compare_versions("0.2.0", "0.1.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("v0.2.0", "0.2.0"), Some(Ordering::Equal));
        assert_eq!(
            compare_versions("0.2.0", "0.2.0-rc1"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("0.2.0-rc2", "0.2.0-rc1"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("0.2.0+build", "0.2.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(compare_versions("not-a-version", "0.2.0"), None);
        assert_eq!(
            Version::parse("0.2.0-rc.1"),
            Some(Version {
                numbers: vec![0, 2, 0],
                pre: Some("rc.1".into()),
            })
        );
    }

    #[test]
    fn sources_require_https_or_explicit_loopback_opt_in() {
        assert!(validate_source("https://releases.example.com/jani", false).is_ok());
        assert!(validate_source("http://127.0.0.1:8080/releases/", false).is_err());
        assert_eq!(
            validate_source("http://127.0.0.1:8080/releases/", true).unwrap(),
            "http://127.0.0.1:8080/releases"
        );
        assert!(validate_source("http://localhost:9000", true).is_ok());
        assert!(validate_source("http://[::1]:9000/releases", true).is_ok());
        assert!(validate_source("http://releases.example.com", true).is_err());
        assert!(validate_source("ftp://releases.example.com", true).is_err());
        assert!(validate_source("https://token@releases.example.com", false).is_err());
        assert!(validate_source("https://releases.example.com?token=x", false).is_err());
        assert!(validate_source("releases.example.com", false).is_err());
    }

    #[test]
    fn artifact_names_cannot_escape_the_release_directory() {
        assert!(safe_file_name("jani-0.2.0-x86_64-pc-windows-msvc.exe"));
        assert!(!safe_file_name("../jani.exe"));
        assert!(!safe_file_name("dir/jani.exe"));
        assert!(!safe_file_name("dir\\jani.exe"));
        assert!(!safe_file_name("jani.exe:stream"));
        assert!(!safe_file_name(".hidden.exe"));
        assert!(safe_file_name("jani"));
    }

    #[test]
    fn manifests_reject_dirty_wrong_platform_and_bad_digests() {
        let mut sample = manifest();
        assert!(validate_manifest(
            &sample,
            "windows",
            "x86_64-pc-windows-msvc",
            "v0.2.0",
            "test"
        )
        .is_ok());
        sample.dirty = true;
        assert!(validate_manifest(
            &sample,
            "windows",
            "x86_64-pc-windows-msvc",
            "v0.2.0",
            "test"
        )
        .is_err());
        let mut sample = manifest();
        assert!(validate_manifest(
            &sample,
            "linux",
            "x86_64-unknown-linux-gnu",
            "v0.2.0",
            "test"
        )
        .is_err());
        let mut artifact = sample.artifacts.remove(0);
        artifact.sha256 = "xyz".into();
        assert!(validate_artifact(&artifact, "x86_64-pc-windows-msvc").is_err());
    }
}
