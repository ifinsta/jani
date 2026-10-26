use crate::config::{Config, JResult};
use crate::{process, types};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CHANNEL_SCHEMA: &str = "jani.release-channel/v2";
const MANIFEST_SCHEMA: &str = "jani.release/v2";
const UPDATE_SCHEMA: &str = "jani.update/v1";
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const FETCH_ATTEMPTS: u64 = 4;

pub fn command(cfg: &Config) -> JResult<()> {
    match cfg.sub.as_deref() {
        None | Some("check") => check(cfg),
        Some("install") => install(cfg),
        Some("watch") => watch(cfg),
        Some(other) => Err(format!("unknown update subcommand '{other}'")),
    }
}

fn check(cfg: &Config) -> JResult<()> {
    let source = source(cfg)?;
    let release = fetch_release(&source)?;
    let current_exe = current_exe();
    let states = inspect_targets(cfg, &release, current_exe.as_deref())?;
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
        "artifact": artifact_json(&release.artifact),
        "updateAvailable": update_available,
        "targets": states.iter().map(TargetState::json).collect::<Vec<_>>(),
    });
    print_json(&report)
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
    validate_source(source)
}

fn validate_source(raw: &str) -> JResult<String> {
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
    if scheme == "http" && !loopback_authority(authority) {
        return Err("plain-http update sources are allowed only on localhost".into());
    }
    Ok(source.to_string())
}

fn loopback_authority(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return false;
        };
        if !(tail.is_empty() || tail.starts_with(':')) {
            return false;
        }
        host
    } else {
        authority
            .rsplit_once(':')
            .map(|(host, _)| host)
            .unwrap_or(authority)
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn host_platform() -> JResult<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(("windows", "x86_64-pc-windows-msvc")),
        ("windows", "aarch64") => Ok(("windows", "aarch64-pc-windows-msvc")),
        ("linux", "x86_64") => Ok(("linux", "x86_64-unknown-linux-gnu")),
        ("linux", "aarch64") => Ok(("linux", "aarch64-unknown-linux-gnu")),
        ("macos", "x86_64") => Ok(("macos", "x86_64-apple-darwin")),
        ("macos", "aarch64") => Ok(("macos", "aarch64-apple-darwin")),
        (os, arch) => Err(format!(
            "no release channel mapping for {os}/{arch}; this updater cannot select an artifact"
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Channel {
    schema_version: String,
    #[serde(rename = "type")]
    kind: String,
    platform: String,
    latest: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
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
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    artifact: Artifact,
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
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
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

fn retry_base() -> Duration {
    std::env::var("JANI_UPDATE_RETRY_BASE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(400))
}

fn retry_delay(attempt: u64) -> Duration {
    let factor = 1u64 << attempt.saturating_sub(1).min(6);
    retry_base()
        .saturating_mul(factor as u32)
        .min(Duration::from_secs(8))
}

fn fetch_release(source: &str) -> JResult<Release> {
    let (platform, target) = host_platform()?;
    let origin = Origin::new()?;
    let channel_url = format!("{source}/agent/{platform}/latest.json");
    let channel: Channel = origin.get_json(&channel_url)?;
    if channel.schema_version != CHANNEL_SCHEMA {
        return Err(format!(
            "unsupported update channel schema '{}' at {channel_url}",
            channel.schema_version
        ));
    }
    if channel.kind != "agent" || channel.platform != platform {
        return Err(format!(
            "update channel at {channel_url} is for {} {}; expected agent {platform}",
            channel.kind, channel.platform
        ));
    }
    if !safe_component(&channel.latest) {
        return Err("update channel latest version is not a safe path component".into());
    }
    let manifest_url = format!("{source}/agent/{platform}/{}/manifest.json", channel.latest);
    let manifest: Manifest = origin.get_json(&manifest_url)?;
    validate_manifest(&manifest, platform, target, &channel.latest, &manifest_url)?;
    let artifact = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.target == target)
        .cloned()
        .ok_or_else(|| format!("release {} has no artifact for {target}", manifest.version))?;
    validate_artifact(&artifact, target)?;
    Ok(Release {
        platform: platform.to_string(),
        target: target.to_string(),
        tag: manifest.tag.clone(),
        version: manifest.version.clone(),
        commit: manifest.commit.clone(),
        built_at: manifest.built_at.clone(),
        artifact,
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
    if manifest.kind != "agent" || manifest.platform != platform {
        return Err(format!(
            "release manifest at {url} is for {} {}; expected agent {platform}",
            manifest.kind, manifest.platform
        ));
    }
    if manifest.package != "jani" {
        return Err(format!(
            "release manifest at {url} packages '{}'; expected jani",
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
}

impl TargetState {
    fn json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "currentVersion": self.current,
            "status": self.status,
            "updateAvailable": self.update_available,
            "restartRequired": self.restart_required,
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
    let mut unique: Vec<PathBuf> = Vec::new();
    for target in true_targets {
        let duplicate = unique.iter().any(|existing| {
            if cfg!(windows) {
                existing
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&target.to_string_lossy())
            } else {
                existing == &target
            }
        });
        if !duplicate {
            unique.push(target);
        }
    }
    Ok(unique)
}

fn inspect_targets(
    cfg: &Config,
    release: &Release,
    current_exe: Option<&Path>,
) -> JResult<Vec<TargetState>> {
    Ok(resolve_targets(cfg)?
        .into_iter()
        .map(|path| inspect_target(path, release, current_exe, cfg.update_force))
        .collect())
}

fn inspect_target(
    path: PathBuf,
    release: &Release,
    current_exe: Option<&Path>,
    force: bool,
) -> TargetState {
    let restart_required = current_exe
        .and_then(|exe| fs::canonicalize(&path).ok().map(|path| path == exe))
        .unwrap_or(false);
    if path.exists() && !path.is_file() {
        return TargetState {
            path,
            current: None,
            error: Some("target exists but is not a regular file".into()),
            update_available: false,
            status: "error",
            restart_required,
        };
    }
    let current = if path.exists() {
        match binary_version(&path) {
            Ok(version) => Some(version),
            Err(error) => {
                return TargetState {
                    path,
                    current: None,
                    error: Some(error),
                    update_available: false,
                    status: "error",
                    restart_required,
                }
            }
        }
    } else {
        None
    };
    let comparison = current
        .as_deref()
        .and_then(|current| compare_versions(&release.version, current));
    let update_available = force || current.is_none() || comparison == Some(Ordering::Greater);
    let status = if update_available {
        "update-available"
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
    }
}

fn binary_version(path: &Path) -> JResult<String> {
    let cwd = path
        .parent()
        .filter(|parent| parent.is_dir())
        .unwrap_or_else(|| Path::new("."));
    let mut command = Command::new(path);
    command.arg("--version");
    let output = process::execute(&mut command, cwd, 10)
        .map_err(|e| format!("cannot run {} --version: {e}", path.display()))?;
    if output.code != Some(0) {
        return Err(format!(
            "{} --version exited with {:?}; refusing to replace an unrecognized binary",
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
}

fn install_once(cfg: &Config, source: &str) -> JResult<InstallOutcome> {
    let release = fetch_release(source)?;
    let current_exe = current_exe();
    let states = inspect_targets(cfg, &release, current_exe.as_deref())?;
    let needs_install = states
        .iter()
        .any(|state| state.update_available && state.error.is_none());
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut installed = 0usize;
    let mut artifact_verified = false;

    if needs_install {
        let url = format!(
            "{source}/agent/{}/{}/{}",
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
        let digest = types::sha256_hex(&bytes);
        if !digest.eq_ignore_ascii_case(&release.artifact.sha256) {
            return Err(format!(
                "downloaded artifact hash {digest} does not match manifest {}",
                release.artifact.sha256
            ));
        }
        artifact_verified = true;

        for state in &states {
            if !state.update_available {
                results.push(state.json());
                continue;
            }
            if let Some(error) = &state.error {
                failures.push(format!("{}: {error}", state.path.display()));
                results.push(state.json());
                continue;
            }
            match install_target(&state.path, &bytes, &release) {
                Ok(result) => {
                    installed += 1;
                    results.push(result);
                }
                Err(error) => {
                    failures.push(format!("{}: {error}", state.path.display()));
                    results.push(json!({
                        "path": state.path.display().to_string(),
                        "currentVersion": state.current,
                        "status": "error",
                        "updateAvailable": true,
                        "restartRequired": state.restart_required,
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

    let status = if !failures.is_empty() {
        if installed > 0 {
            "partial"
        } else {
            "error"
        }
    } else if installed > 0 {
        "installed"
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
        "artifact": artifact_json(&release.artifact),
        "artifactVerified": artifact_verified,
        "status": status,
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
        failure: if failures.is_empty() {
            None
        } else {
            Some(failures.join("; "))
        },
    })
}

fn install_target(path: &Path, bytes: &[u8], release: &Release) -> JResult<Value> {
    let previous = binary_version(path).ok();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("target has no usable file name")?;
    let parent = path.parent().ok_or("target has no parent directory")?;
    let id = process::unique_id();
    let candidate = parent.join(format!(".{name}.jani-update-{id}.tmp"));
    write_candidate(&candidate, bytes)?;
    let verified = (|| -> JResult<()> {
        let mut last = String::new();
        for attempt in 1..=3 {
            match binary_version(&candidate) {
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
    let backup = match replace_binary(&candidate, path) {
        Ok(backup) => backup,
        Err(error) => {
            let _ = fs::remove_file(&candidate);
            return Err(error);
        }
    };
    Ok(json!({
        "path": path.display().to_string(),
        "previousVersion": previous,
        "newVersion": release.version,
        "status": "installed",
        "updateAvailable": false,
        "restartRequired": current_exe()
            .and_then(|exe| fs::canonicalize(path).ok().map(|target| target == exe))
            .unwrap_or(false),
        "sha256": release.artifact.sha256,
        "bytes": bytes.len(),
        "backup": backup.map(|path| path.display().to_string()),
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
    file.write_all(bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("cannot flush {}: {e}", path.display()))?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot mark {} executable: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(windows)]
fn replace_binary(candidate: &Path, target: &Path) -> JResult<Option<PathBuf>> {
    if !target.exists() {
        fs::rename(candidate, target)
            .map_err(|e| format!("cannot install {}: {e}", target.display()))?;
        return Ok(None);
    }
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("jani");
    let backup = target.with_file_name(format!(".{name}.jani-old-{}", process::unique_id()));
    // Windows locks the image of a running executable but still permits renaming it.
    fs::rename(target, &backup).map_err(|e| {
        format!(
            "cannot move the installed binary {} aside: {e}",
            target.display()
        )
    })?;
    match fs::rename(candidate, target) {
        Ok(()) => {
            if fs::remove_file(&backup).is_ok() {
                Ok(None)
            } else {
                Ok(Some(backup))
            }
        }
        Err(error) => {
            let restored = fs::rename(&backup, target);
            match restored {
                Ok(()) => Err(format!(
                    "cannot install the verified update: {error}; the previous binary was restored"
                )),
                Err(restore_error) => Err(format!(
                    "cannot install the verified update: {error}; restoring {} also failed: {restore_error}; backup kept at {}",
                    target.display(),
                    backup.display()
                )),
            }
        }
    }
}

#[cfg(not(windows))]
fn replace_binary(candidate: &Path, target: &Path) -> JResult<Option<PathBuf>> {
    fs::rename(candidate, target)
        .map_err(|e| format!("cannot install {}: {e}", target.display()))?;
    Ok(None)
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
    let dir = cfg.state_dir.join("updates");
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

    fn manifest() -> Manifest {
        Manifest {
            schema_version: MANIFEST_SCHEMA.into(),
            kind: "agent".into(),
            platform: "windows".into(),
            package: "jani".into(),
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
        }
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
    fn sources_require_https_or_loopback() {
        assert!(validate_source("https://releases.example.com/jani").is_ok());
        assert_eq!(
            validate_source("http://127.0.0.1:8080/releases/").unwrap(),
            "http://127.0.0.1:8080/releases"
        );
        assert!(validate_source("http://localhost:9000").is_ok());
        assert!(validate_source("http://[::1]:9000/releases").is_ok());
        assert!(validate_source("http://releases.example.com").is_err());
        assert!(validate_source("ftp://releases.example.com").is_err());
        assert!(validate_source("https://token@releases.example.com").is_err());
        assert!(validate_source("https://releases.example.com?token=x").is_err());
        assert!(validate_source("releases.example.com").is_err());
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
