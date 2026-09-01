//! Conservative local resolution for the Pi coding-agent harness.
//!
//! The session-file reader below is shared with omp (`src/omp.rs`): omp is a
//! fork of Pi and still writes the same JSONL v3 transcript, so the branch
//! walk, model evidence, and usage counters are one implementation with two
//! callers. Only the credential store and the model catalog diverged, and
//! those stay in each harness's own module.

use crate::model::{BillingTarget, CacheTotals, CacheUsage, ContextUsage, Provider, Resolution};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

pub const MAX_SESSION_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_SESSION_LINE_BYTES: usize = 1024 * 1024;
const MAX_AUTH_BYTES: u64 = 1024 * 1024;
const MAX_MODELS_BYTES: u64 = 8 * 1024 * 1024;
const SUPPORTED_SESSION_VERSION: u64 = 3;

#[derive(Debug, Clone)]
pub struct PiPaths {
    pub auth: PathBuf,
    pub models_config: PathBuf,
    pub models_store: PathBuf,
    pub sessions: PathBuf,
}

impl PiPaths {
    pub fn from_env() -> Option<Self> {
        let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
        let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .map(|path| expand_tilde(path, &home))
            .unwrap_or_else(|| home.join(".pi/agent"));
        let sessions = std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
            .map(PathBuf::from)
            .map(|path| expand_tilde(path, &home))
            .unwrap_or_else(|| agent_dir.join("sessions"));
        Some(Self {
            auth: agent_dir.join("auth.json"),
            models_config: agent_dir.join("models.json"),
            models_store: agent_dir.join("models-store.json"),
            sessions,
        })
    }

    #[cfg(test)]
    pub fn from_dirs(agent_dir: impl Into<PathBuf>, sessions: impl Into<PathBuf>) -> Self {
        let agent_dir = agent_dir.into();
        Self {
            auth: agent_dir.join("auth.json"),
            models_config: agent_dir.join("models.json"),
            models_store: agent_dir.join("models-store.json"),
            sessions: sessions.into(),
        }
    }
}

fn expand_tilde(path: PathBuf, home: &Path) -> PathBuf {
    if path == Path::new("~") {
        return home.to_path_buf();
    }
    path.strip_prefix("~/")
        .map(|suffix| home.join(suffix))
        .unwrap_or(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvidence {
    pub provider_id: String,
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLookup {
    Found(SessionEvidence),
    Missing,
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CredentialKind {
    ApiKey,
    Oauth { account_id: Option<String> },
    Unknown,
}

#[derive(Debug, Deserialize)]
struct CredentialMetadata {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "accountId")]
    account_id: Option<String>,
}

pub fn resolve(
    session_path: Option<&str>,
    paths: Option<PiPaths>,
    canonical_codex_account_id: impl FnOnce() -> Option<String>,
) -> Resolution {
    resolve_with_session(session_path, paths, canonical_codex_account_id).resolution
}

pub(crate) struct PiRoute {
    pub resolution: Resolution,
    pub session: Option<SessionEvidence>,
    pub context: Option<ContextUsage>,
}

pub(crate) fn resolve_with_session(
    session_path: Option<&str>,
    paths: Option<PiPaths>,
    canonical_codex_account_id: impl FnOnce() -> Option<String>,
) -> PiRoute {
    let Some(session_path) = session_path.filter(|path| !path.is_empty()) else {
        return PiRoute {
            resolution: Resolution::Indeterminate,
            session: None,
            context: None,
        };
    };
    let Some(paths) = paths else {
        return PiRoute {
            resolution: Resolution::Indeterminate,
            session: None,
            context: None,
        };
    };
    let DetailedSessionLookup::Found(parsed) =
        lookup_session_detailed(&paths, Path::new(session_path))
    else {
        return PiRoute {
            resolution: Resolution::Indeterminate,
            session: None,
            context: None,
        };
    };
    let session = parsed.evidence.clone();
    let context = session_context(&paths, &session, &parsed);
    let resolution = if parsed.message_provider_id.as_deref() != Some(&session.provider_id) {
        Resolution::Indeterminate
    } else {
        match read_auth_metadata(&paths.auth)
            .ok()
            .and_then(|auth| auth.get(&session.provider_id).cloned())
        {
            None => Resolution::Indeterminate,
            Some(credential) => match credential {
                CredentialKind::ApiKey => Resolution::NoSubscription,
                CredentialKind::Oauth { account_id } if session.provider_id == "openai-codex" => {
                    let canonical_account_id = canonical_codex_account_id();
                    if account_id.as_deref().is_some()
                        && account_id.as_deref() == canonical_account_id.as_deref()
                    {
                        Resolution::Subscription(BillingTarget::original_four(Provider::Codex))
                    } else {
                        Resolution::Indeterminate
                    }
                }
                CredentialKind::Oauth { .. } | CredentialKind::Unknown => Resolution::Indeterminate,
            },
        }
    };
    PiRoute {
        resolution,
        session: Some(session),
        context,
    }
}

pub fn lookup_session(paths: &PiPaths, supplied_path: &Path) -> SessionLookup {
    match lookup_session_detailed(paths, supplied_path) {
        DetailedSessionLookup::Found(parsed) => SessionLookup::Found(parsed.evidence),
        DetailedSessionLookup::Missing => SessionLookup::Missing,
        DetailedSessionLookup::Unreadable => SessionLookup::Unreadable,
    }
}

#[derive(Debug)]
pub(crate) struct ParsedSession {
    pub(crate) evidence: SessionEvidence,
    pub(crate) message_provider_id: Option<String>,
    pub(crate) session_id: String,
    pub(crate) context_tokens: Option<u64>,
    pub(crate) latest_usage: UsageCounters,
    pub(crate) usage_totals: UsageCounters,
    pub(crate) cache_activity: Option<CacheActivity>,
    /// Latest `credential_pin` hash on the active branch, for the provider the
    /// session is talking to. omp writes it; Pi does not.
    pub(crate) credential_pin: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CacheActivity {
    pub(crate) ttl_seconds: u64,
    pub(crate) last_activity_unix: u64,
}

pub(crate) enum DetailedSessionLookup {
    Found(Box<ParsedSession>),
    Missing,
    Unreadable,
}

fn lookup_session_detailed(paths: &PiPaths, supplied_path: &Path) -> DetailedSessionLookup {
    lookup_session_in(&paths.sessions, supplied_path)
}

/// Read one transcript, refusing anything outside `sessions_root`.
pub(crate) fn lookup_session_in(
    sessions_root: &Path,
    supplied_path: &Path,
) -> DetailedSessionLookup {
    if !supplied_path.is_absolute() {
        return DetailedSessionLookup::Unreadable;
    }
    let root = match fs::canonicalize(sessions_root) {
        Ok(root) => root,
        Err(_) => return DetailedSessionLookup::Missing,
    };
    let path = match fs::canonicalize(supplied_path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DetailedSessionLookup::Missing;
        }
        Err(_) => return DetailedSessionLookup::Unreadable,
    };
    if !path.starts_with(&root)
        || path.extension().and_then(|value| value.to_str()) != Some("jsonl")
    {
        return DetailedSessionLookup::Unreadable;
    }

    parse_session_file(&path)
}

fn parse_session_file(path: &Path) -> DetailedSessionLookup {
    let bytes = match read_bounded(path, MAX_SESSION_BYTES) {
        Ok(bytes) => bytes,
        Err(_) => return DetailedSessionLookup::Unreadable,
    };
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return DetailedSessionLookup::Unreadable;
    }

    let mut header_id = None;
    let mut entries = Vec::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        if line.len() > MAX_SESSION_LINE_BYTES {
            return DetailedSessionLookup::Unreadable;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            return DetailedSessionLookup::Unreadable;
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("session") => {
                if header_id.is_some()
                    || entry.get("version").and_then(Value::as_u64)
                        != Some(SUPPORTED_SESSION_VERSION)
                {
                    return DetailedSessionLookup::Unreadable;
                }
                let Some(id) = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| valid_id(id))
                else {
                    return DetailedSessionLookup::Unreadable;
                };
                header_id = Some(id.to_string());
            }
            // omp opens its transcripts with a padded `title` record that
            // carries no id and no parent. It is a header, not a branch entry:
            // pushing it would fail the id/parentId walk for every omp session.
            Some("title") => {}
            _ => entries.push(entry),
        }
    }

    let Some(header_id) = header_id else {
        return DetailedSessionLookup::Unreadable;
    };
    if !filename_matches_session_id(path, &header_id) {
        return DetailedSessionLookup::Unreadable;
    }
    let Some(branch) = active_branch(&entries) else {
        return DetailedSessionLookup::Unreadable;
    };
    let Some(evidence) = active_model(&branch) else {
        return DetailedSessionLookup::Unreadable;
    };
    let message_provider_id = branch.iter().rev().find_map(|entry| {
        (entry.get("type").and_then(Value::as_str) == Some("message")
            && entry.pointer("/message/role").and_then(Value::as_str) == Some("assistant"))
        .then(|| nonempty_string(entry.pointer("/message/provider")))
        .flatten()
        .map(str::to_string)
    });
    let context_tokens = context_tokens(&branch);
    let cache_activity = cache_activity(&branch, &evidence.provider_id);
    let credential_pin = credential_pin(&branch, &evidence.provider_id);
    let latest_usage = branch
        .iter()
        .rev()
        .find_map(|entry| assistant_usage(entry).filter(|usage| usage.context_tokens() > 0))
        .unwrap_or_default();
    let mut usage_totals = UsageCounters::default();
    for entry in &entries {
        if let Some(usage) = usage_for_totals(entry) {
            usage_totals.add(usage);
        }
    }
    DetailedSessionLookup::Found(Box::new(ParsedSession {
        evidence,
        message_provider_id,
        session_id: header_id,
        context_tokens,
        latest_usage,
        usage_totals,
        cache_activity,
        credential_pin,
    }))
}

/// Latest account pin recorded for `provider_id` on the active branch.
///
/// omp appends a `credential_pin` entry whenever the serving OAuth account
/// changes, so the last one on the branch names the account that is paying for
/// this session. Pi writes none, which reads back as `None`.
fn credential_pin(branch: &[&Value], provider_id: &str) -> Option<String> {
    branch.iter().rev().find_map(|entry| {
        (entry.get("type").and_then(Value::as_str) == Some("credential_pin")
            && entry.get("provider").and_then(Value::as_str) == Some(provider_id))
        .then(|| nonempty_string(entry.get("hash")))
        .flatten()
        .map(str::to_string)
    })
}

fn active_branch(entries: &[Value]) -> Option<Vec<&Value>> {
    let mut by_id = BTreeMap::new();
    for entry in entries {
        let id = nonempty_string(entry.get("id"))?;
        if by_id.insert(id, entry).is_some() {
            return None;
        }
        if !entry.get("parentId").is_some_and(Value::is_null)
            && entry.get("parentId").and_then(Value::as_str).is_none()
        {
            return None;
        }
    }
    let mut branch = Vec::new();
    let mut current = entries.last()?;
    for _ in 0..entries.len() {
        branch.push(current);
        let Some(parent_id) = current.get("parentId").and_then(Value::as_str) else {
            branch.reverse();
            return Some(branch);
        };
        current = *by_id.get(parent_id)?;
    }
    None
}

fn active_model(branch: &[&Value]) -> Option<SessionEvidence> {
    let mut active = None;
    for entry in branch {
        match entry.get("type").and_then(Value::as_str) {
            Some("model_change") => {
                if let Some(evidence) = model_change_evidence(entry) {
                    active = Some(evidence);
                }
            }
            Some("message")
                if entry.pointer("/message/role").and_then(Value::as_str) == Some("assistant") =>
            {
                let provider = nonempty_string(entry.pointer("/message/provider"))?;
                active = Some(SessionEvidence {
                    provider_id: provider.to_string(),
                    model_id: nonempty_string(entry.pointer("/message/model")).map(str::to_string),
                });
            }
            _ => {}
        }
    }
    active
}

/// The model a `model_change` selects, in either harness's spelling.
///
/// Pi writes `provider` and `modelId` as separate fields; omp writes one
/// `provider/modelId` selector and tags the entry with the role it changed, so
/// a switch of the `smol` or `plan` model is not a switch of the pane's own.
/// An entry in neither shape is skipped rather than treated as evidence.
fn model_change_evidence(entry: &Value) -> Option<SessionEvidence> {
    match entry.get("role").and_then(Value::as_str) {
        None | Some("default") => {}
        Some(_) => return None,
    }
    if let Some(provider) = nonempty_string(entry.get("provider")) {
        return Some(SessionEvidence {
            provider_id: provider.to_string(),
            model_id: nonempty_string(entry.get("modelId")).map(str::to_string),
        });
    }
    let (provider, model) = nonempty_string(entry.get("model"))?.split_once('/')?;
    (!provider.is_empty() && !model.is_empty()).then(|| SessionEvidence {
        provider_id: provider.to_string(),
        model_id: Some(model.to_string()),
    })
}

fn context_tokens(branch: &[&Value]) -> Option<u64> {
    let after_compaction = branch
        .iter()
        .rposition(|entry| entry.get("type").and_then(Value::as_str) == Some("compaction"))
        .map_or(branch, |index| &branch[index + 1..]);
    after_compaction
        .iter()
        .rev()
        .find_map(|entry| assistant_usage(entry).map(|usage| usage.context_tokens()))
        .filter(|tokens| *tokens > 0)
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct UsageCounters {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cache_write_1h: Option<u64>,
    total_tokens: u64,
    context_tokens: u64,
}

impl UsageCounters {
    fn context_tokens(self) -> u64 {
        // omp reports the occupied context directly when the provider makes it
        // authoritative; Pi never does, and both fall back to the sum.
        if self.context_tokens > 0 {
            return self.context_tokens;
        }
        if self.total_tokens > 0 {
            self.total_tokens
        } else {
            self.input
                .saturating_add(self.output)
                .saturating_add(self.cache_read)
                .saturating_add(self.cache_write)
        }
    }

    fn add(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        if self.cache_write_1h.is_some() || other.cache_write_1h.is_some() {
            self.cache_write_1h = Some(
                self.cache_write_1h
                    .unwrap_or_default()
                    .saturating_add(other.cache_write_1h.unwrap_or_default()),
            );
        }
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.context_tokens = self.context_tokens.saturating_add(other.context_tokens);
    }
}

fn assistant_usage(entry: &Value) -> Option<UsageCounters> {
    if entry.get("type").and_then(Value::as_str) != Some("message")
        || entry.pointer("/message/role").and_then(Value::as_str) != Some("assistant")
        || matches!(
            entry.pointer("/message/stopReason").and_then(Value::as_str),
            Some("aborted" | "error")
        )
    {
        return None;
    }
    usage_counters(entry.pointer("/message/usage")?)
}

fn usage_for_totals(entry: &Value) -> Option<UsageCounters> {
    match entry.get("type").and_then(Value::as_str) {
        Some("message")
            if matches!(
                entry.pointer("/message/role").and_then(Value::as_str),
                Some("assistant" | "toolResult")
            ) =>
        {
            usage_counters(entry.pointer("/message/usage")?)
        }
        Some("branch_summary" | "compaction") => usage_counters(entry.get("usage")?),
        _ => None,
    }
}

fn usage_counters(usage: &Value) -> Option<UsageCounters> {
    usage.as_object()?;
    Some(UsageCounters {
        input: usage.get("input").and_then(Value::as_u64).unwrap_or(0),
        output: usage.get("output").and_then(Value::as_u64).unwrap_or(0),
        cache_read: usage.get("cacheRead").and_then(Value::as_u64).unwrap_or(0),
        cache_write: usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0),
        // Pi writes `cacheWrite1h`; omp writes the same split under
        // `cttl.ephemeral1h`. Neither writes the other's spelling.
        cache_write_1h: usage
            .get("cacheWrite1h")
            .and_then(Value::as_u64)
            .or_else(|| usage.pointer("/cttl/ephemeral1h").and_then(Value::as_u64)),
        total_tokens: usage
            .get("totalTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        context_tokens: usage
            .get("contextTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn cache_activity(branch: &[&Value], provider_id: &str) -> Option<CacheActivity> {
    match provider_id {
        "anthropic" => anthropic_cache_activity(branch, provider_id),
        // Codex records no TTL and no expiry, so the estimate is the
        // documented 30 minute prompt cache lifetime anchored to the last
        // request that touched the cache. Pi only reports `cacheRead` for this
        // provider; `cacheWrite` stays 0 even on a warm session.
        "openai-codex" => codex_cache_activity(branch, provider_id),
        _ => None,
    }
}

fn codex_cache_activity(branch: &[&Value], provider_id: &str) -> Option<CacheActivity> {
    let last_activity_unix = branch.iter().rev().find_map(|entry| {
        if entry.pointer("/message/provider").and_then(Value::as_str) != Some(provider_id) {
            return None;
        }
        let usage = assistant_usage(entry)?;
        (usage.cache_read > 0 || usage.cache_write > 0)
            .then(|| message_started_at(entry))
            .flatten()
    })?;
    Some(CacheActivity {
        ttl_seconds: crate::providers::codex::CODEX_PROMPT_CACHE_TTL_SECONDS,
        last_activity_unix,
    })
}

fn anthropic_cache_activity(branch: &[&Value], provider_id: &str) -> Option<CacheActivity> {
    let mut ttl_seconds = None;
    let mut last_activity_unix = None;
    for entry in branch.iter().rev() {
        if entry.pointer("/message/provider").and_then(Value::as_str) != Some(provider_id) {
            continue;
        }
        let Some(usage) = assistant_usage(entry) else {
            continue;
        };
        if usage.cache_read == 0 && usage.cache_write == 0 {
            continue;
        }
        if last_activity_unix.is_none() {
            last_activity_unix = message_started_at(entry);
        }
        if ttl_seconds.is_none() && usage.cache_write > 0 {
            ttl_seconds = usage
                .cache_write_1h
                .map(|one_hour| if one_hour > 0 { 60 * 60 } else { 5 * 60 });
        }
        if let (Some(ttl_seconds), Some(last_activity_unix)) = (ttl_seconds, last_activity_unix) {
            return Some(CacheActivity {
                ttl_seconds,
                last_activity_unix,
            });
        }
    }
    None
}

fn message_started_at(entry: &Value) -> Option<u64> {
    entry
        .pointer("/message/timestamp")
        .and_then(Value::as_u64)
        .filter(|milliseconds| *milliseconds >= 1_000_000_000_000)
        .map(|milliseconds| milliseconds / 1_000)
}

#[derive(Deserialize)]
struct ModelsEntry {
    #[serde(default)]
    models: Vec<ModelMetadata>,
}

#[derive(Deserialize)]
struct ModelMetadata {
    id: String,
    #[serde(rename = "contextWindow")]
    context_window: Option<u64>,
}

fn session_context(
    paths: &PiPaths,
    session: &SessionEvidence,
    parsed: &ParsedSession,
) -> Option<ContextUsage> {
    // Pi composes and validates models.json as a whole. A local model or
    // override can replace contextWindow, but the effective value is not
    // recorded in session JSONL or models-store.json. Do not publish a
    // percentage from the uncomposed catalog when that file exists.
    match paths.models_config.try_exists() {
        Ok(false) => {}
        Ok(true) | Err(_) => return None,
    }
    let model_id = session.model_id.as_deref()?;
    let context_tokens = parsed.context_tokens?;
    let bytes = read_bounded(&paths.models_store, MAX_MODELS_BYTES).ok()?;
    let stores: BTreeMap<String, ModelsEntry> = serde_json::from_slice(&bytes).ok()?;
    let context_window = stores
        .get(&session.provider_id)?
        .models
        .iter()
        .find(|model| model.id == model_id)?
        .context_window
        .filter(|window| *window > 0)?;
    context_usage(parsed, context_tokens, context_window)
}

/// Assemble the published context/cache view from an already resolved window.
///
/// Split out because omp reads the same transcript but resolves its context
/// window from its own catalog.
pub(crate) fn context_usage(
    parsed: &ParsedSession,
    context_tokens: u64,
    context_window: u64,
) -> Option<ContextUsage> {
    let used_percent = (context_tokens as f64 / context_window as f64 * 100.0).clamp(0.0, 100.0);
    let cache = if parsed.usage_totals.cache_read > 0 || parsed.usage_totals.cache_write > 0 {
        CacheUsage::from_token_counts(
            parsed.latest_usage.input,
            parsed.latest_usage.cache_read,
            parsed.latest_usage.cache_write,
        )
        .map(|cache| {
            let cache = cache.with_session_totals(
                CacheTotals::from_token_counts(
                    parsed.usage_totals.input,
                    parsed.usage_totals.cache_read,
                    parsed.usage_totals.cache_write,
                ),
                parsed.session_id.clone(),
                0,
            );
            if let Some(activity) = parsed.cache_activity {
                cache.with_ttl_estimate(activity.ttl_seconds, activity.last_activity_unix)
            } else {
                cache
            }
        })
    } else {
        None
    };
    ContextUsage::new(used_percent)
        .ok()
        .map(|context| context.with_cache(cache))
}

fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds read limit",
        ));
    }
    Ok(bytes)
}

fn read_auth_metadata(path: &Path) -> Result<BTreeMap<String, CredentialKind>, ()> {
    let metadata = fs::metadata(path).map_err(|_| ())?;
    if !metadata.is_file() || metadata.len() > MAX_AUTH_BYTES {
        return Err(());
    }
    // Deserialize only credential metadata. Unknown fields, including access
    // and refresh tokens, are skipped and never enter an owned Rust value.
    let reader = BufReader::new(File::open(path).map_err(|_| ())?.take(MAX_AUTH_BYTES + 1));
    let entries: BTreeMap<String, CredentialMetadata> =
        serde_json::from_reader(reader).map_err(|_| ())?;
    Ok(entries
        .into_iter()
        .map(|(provider, credential)| {
            let kind = match credential.kind.as_str() {
                "api_key" => CredentialKind::ApiKey,
                "oauth" => CredentialKind::Oauth {
                    account_id: credential.account_id.filter(|value| !value.is_empty()),
                },
                _ => CredentialKind::Unknown,
            };
            (provider, kind)
        })
        .collect())
}

fn nonempty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && id.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
}

fn filename_matches_session_id(path: &Path, session_id: &str) -> bool {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return false;
    };
    stem == session_id || stem.ends_with(&format!("_{session_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi")
            .join(name)
    }

    #[test]
    fn pi_environment_paths_expand_tilde_like_pi() {
        let home = Path::new("/home/tester");
        assert_eq!(
            expand_tilde(PathBuf::from("~/.pi/custom"), home),
            home.join(".pi/custom")
        );
        assert_eq!(
            expand_tilde(PathBuf::from("/var/pi"), home),
            PathBuf::from("/var/pi")
        );
    }

    fn install_fixture(root: &Path, fixture_name: &str, session_id: &str) -> (PiPaths, PathBuf) {
        let agent = root.join("agent");
        let sessions = root.join("sessions/project");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&agent).unwrap();
        fs::copy(fixture("auth-matching.json"), agent.join("auth.json")).unwrap();
        let path = sessions.join(format!("2026-08-29T00-00-00-000Z_{session_id}.jsonl"));
        fs::copy(fixture(fixture_name), &path).unwrap();
        (PiPaths::from_dirs(agent, root.join("sessions")), path)
    }

    #[test]
    fn later_assistant_wins_over_an_earlier_model_change() {
        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(root.path(), "session-codex.jsonl", "session-codex");
        assert_eq!(
            lookup_session(&paths, &path),
            SessionLookup::Found(SessionEvidence {
                provider_id: "openai-codex".to_string(),
                model_id: Some("model-b".to_string()),
            })
        );
    }

    #[test]
    fn later_model_change_wins_over_an_earlier_assistant() {
        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(
            root.path(),
            "session-switched-xai.jsonl",
            "session-switched-xai",
        );
        assert_eq!(
            lookup_session(&paths, &path),
            SessionLookup::Found(SessionEvidence {
                provider_id: "xai".to_string(),
                model_id: Some("grok-4.6".to_string()),
            })
        );
    }

    #[test]
    fn model_change_without_a_matching_message_cannot_select_a_collector() {
        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(
            root.path(),
            "session-switched-codex-unconfirmed.jsonl",
            "session-switched-codex-unconfirmed",
        );
        let route = resolve_with_session(path.to_str(), Some(paths), || {
            Some("account-same".to_string())
        });
        assert_eq!(route.resolution, Resolution::Indeterminate);
        assert_eq!(
            route.session,
            Some(SessionEvidence {
                provider_id: "openai-codex".to_string(),
                model_id: Some("model-b".to_string()),
            })
        );
    }

    #[test]
    fn active_branch_ignores_a_later_abandoned_model() {
        let entries = vec![
            serde_json::json!({"type":"model_change","id":"root","parentId":null,"provider":"openai-codex","modelId":"model-a"}),
            serde_json::json!({"type":"model_change","id":"abandoned","parentId":"root","provider":"xai","modelId":"model-x"}),
            serde_json::json!({"type":"message","id":"active","parentId":"root","message":{"role":"assistant","provider":"openai-codex","model":"model-b","stopReason":"stop","usage":{"input":25,"output":5,"cacheRead":70,"cacheWrite":0,"totalTokens":100}}}),
        ];
        let branch = active_branch(&entries).unwrap();
        assert_eq!(
            active_model(&branch),
            Some(SessionEvidence {
                provider_id: "openai-codex".to_string(),
                model_id: Some("model-b".to_string()),
            })
        );
        assert_eq!(context_tokens(&branch), Some(100));
    }

    #[test]
    fn context_is_unknown_after_compaction_until_a_valid_response() {
        let entries = vec![
            serde_json::json!({"type":"message","id":"before","parentId":null,"message":{"role":"assistant","provider":"openai-codex","model":"model-a","stopReason":"stop","usage":{"totalTokens":190}}}),
            serde_json::json!({"type":"compaction","id":"compact","parentId":"before","usage":{"input":10,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":12}}),
            serde_json::json!({"type":"message","id":"failed","parentId":"compact","message":{"role":"assistant","provider":"openai-codex","model":"model-a","stopReason":"error","usage":{"totalTokens":25}}}),
        ];
        let branch = active_branch(&entries).unwrap();
        assert_eq!(context_tokens(&branch), None);

        let mut completed = entries;
        completed.push(serde_json::json!({"type":"message","id":"after","parentId":"failed","message":{"role":"assistant","provider":"openai-codex","model":"model-a","stopReason":"stop","usage":{"input":20,"output":5,"cacheRead":80,"cacheWrite":0,"totalTokens":105}}}));
        let branch = active_branch(&completed).unwrap();
        assert_eq!(context_tokens(&branch), Some(105));
    }

    #[test]
    fn anthropic_cache_ttl_uses_the_recorded_bucket_and_latest_request_start() {
        let entries = vec![
            serde_json::json!({"type":"message","id":"write","parentId":null,"message":{"role":"assistant","provider":"anthropic","model":"model-a","stopReason":"stop","timestamp":1_700_000_000_000_u64,"usage":{"cacheRead":0,"cacheWrite":100,"cacheWrite1h":100,"totalTokens":100}}}),
            serde_json::json!({"type":"message","id":"read","parentId":"write","message":{"role":"assistant","provider":"anthropic","model":"model-a","stopReason":"stop","timestamp":1_700_000_060_000_u64,"usage":{"cacheRead":100,"cacheWrite":0,"cacheWrite1h":0,"totalTokens":100}}}),
        ];
        let branch = active_branch(&entries).unwrap();
        let activity = cache_activity(&branch, "anthropic").unwrap();
        assert_eq!(activity.ttl_seconds, 60 * 60);
        assert_eq!(activity.last_activity_unix, 1_700_000_060);
        assert!(cache_activity(&branch, "xai").is_none());

        let short = vec![
            serde_json::json!({"type":"message","id":"write","parentId":null,"message":{"role":"assistant","provider":"anthropic","model":"model-a","stopReason":"stop","timestamp":1_700_000_000_000_u64,"usage":{"cacheRead":0,"cacheWrite":100,"cacheWrite1h":0,"totalTokens":100}}}),
        ];
        let branch = active_branch(&short).unwrap();
        assert_eq!(
            cache_activity(&branch, "anthropic").unwrap().ttl_seconds,
            5 * 60
        );
    }

    #[test]
    fn codex_cache_ttl_estimates_thirty_minutes_from_the_latest_cached_request() {
        let entries = vec![
            serde_json::json!({"type":"message","id":"first","parentId":null,"message":{"role":"assistant","provider":"openai-codex","model":"model-a","stopReason":"stop","timestamp":1_700_000_000_000_u64,"usage":{"cacheRead":0,"cacheWrite":0,"totalTokens":100}}}),
            serde_json::json!({"type":"message","id":"cached","parentId":"first","message":{"role":"assistant","provider":"openai-codex","model":"model-a","stopReason":"stop","timestamp":1_700_000_060_000_u64,"usage":{"cacheRead":800,"cacheWrite":0,"totalTokens":900}}}),
        ];
        let branch = active_branch(&entries).unwrap();
        let activity = cache_activity(&branch, "openai-codex").unwrap();
        assert_eq!(
            activity.ttl_seconds,
            crate::providers::codex::CODEX_PROMPT_CACHE_TTL_SECONDS
        );
        assert_eq!(activity.last_activity_unix, 1_700_000_060);

        let cold = vec![entries[0].clone()];
        let branch = active_branch(&cold).unwrap();
        assert!(cache_activity(&branch, "openai-codex").is_none());
    }

    #[test]
    fn models_config_makes_catalog_context_indeterminate() {
        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(
            root.path(),
            "session-codex-usage.jsonl",
            "session-codex-usage",
        );
        fs::write(
            &paths.models_store,
            r#"{"openai-codex":{"models":[{"id":"model-b","contextWindow":200}]}}"#,
        )
        .unwrap();
        fs::write(
            &paths.models_config,
            r#"{"providers":{"openai-codex":{"modelOverrides":{"model-b":{"contextWindow":400}}}}}"#,
        )
        .unwrap();
        let route = resolve_with_session(path.to_str(), Some(paths), || {
            Some("account-same".to_string())
        });
        assert!(route.context.is_none());
    }

    #[test]
    fn exact_account_match_routes_only_to_canonical_codex() {
        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(root.path(), "session-codex.jsonl", "session-codex");
        assert_eq!(
            resolve(path.to_str(), Some(paths.clone()), || {
                Some("account-same".to_string())
            }),
            Resolution::Subscription(BillingTarget::original_four(Provider::Codex))
        );
        assert_eq!(
            resolve(path.to_str(), Some(paths.clone()), || {
                Some("account-other".to_string())
            }),
            Resolution::Indeterminate
        );
        assert_eq!(
            resolve(path.to_str(), Some(paths), || None),
            Resolution::Indeterminate
        );
    }

    #[test]
    fn exact_provider_credential_is_required() {
        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(root.path(), "session-codex.jsonl", "session-codex");
        fs::copy(fixture("auth-different-provider.json"), &paths.auth).unwrap();
        assert_eq!(
            resolve(path.to_str(), Some(paths.clone()), || {
                Some("account-same".to_string())
            }),
            Resolution::Indeterminate
        );
        fs::copy(fixture("auth-no-account.json"), &paths.auth).unwrap();
        assert_eq!(
            resolve(path.to_str(), Some(paths), || {
                Some("account-same".to_string())
            }),
            Resolution::Indeterminate
        );

        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(root.path(), "session-codex.jsonl", "session-codex");
        fs::write(
            &paths.auth,
            r#"{"OpenAI-Codex":{"type":"oauth","accountId":"account-same"}}"#,
        )
        .unwrap();
        assert_eq!(
            resolve(path.to_str(), Some(paths), || {
                Some("account-same".to_string())
            }),
            Resolution::Indeterminate
        );
    }

    #[test]
    fn api_key_is_payg_but_unsupported_oauth_is_indeterminate() {
        let root = tempdir().unwrap();
        let (paths, payg) = install_fixture(root.path(), "session-payg.jsonl", "session-payg");
        fs::copy(fixture("auth-payg.json"), &paths.auth).unwrap();
        assert_eq!(
            resolve(payg.to_str(), Some(paths.clone()), || {
                panic!("PAYG resolution must not inspect canonical Codex auth")
            }),
            Resolution::NoSubscription
        );

        for (fixture_name, session_id) in [
            ("session-xai.jsonl", "session-xai"),
            ("session-anthropic.jsonl", "session-anthropic"),
        ] {
            let target = payg
                .parent()
                .unwrap()
                .join(format!("2026-08-29T00-00-00-000Z_{session_id}.jsonl"));
            fs::copy(fixture(fixture_name), &target).unwrap();
            fs::copy(fixture("auth-unsupported-oauth.json"), &paths.auth).unwrap();
            assert_eq!(
                resolve(target.to_str(), Some(paths.clone()), || {
                    panic!("unsupported OAuth must not inspect canonical Codex auth")
                }),
                Resolution::Indeterminate
            );
        }
    }

    #[test]
    fn missing_outside_and_mismatched_sessions_fail_closed() {
        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(root.path(), "session-codex.jsonl", "session-codex");
        assert_eq!(
            lookup_session(&paths, &path.with_file_name("missing.jsonl")),
            SessionLookup::Missing
        );

        let outside = root.path().join("outside_session-codex.jsonl");
        fs::copy(fixture("session-codex.jsonl"), &outside).unwrap();
        assert_eq!(lookup_session(&paths, &outside), SessionLookup::Unreadable);

        let mismatched = path.with_file_name("2026-08-29T00-00-00-000Z_other-id.jsonl");
        fs::copy(fixture("session-codex.jsonl"), &mismatched).unwrap();
        assert_eq!(
            lookup_session(&paths, &mismatched),
            SessionLookup::Unreadable
        );
    }

    #[test]
    fn malformed_tail_unknown_version_and_long_lines_fail_closed() {
        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(
            root.path(),
            "session-malformed-tail.jsonl",
            "session-malformed",
        );
        assert_eq!(lookup_session(&paths, &path), SessionLookup::Unreadable);

        let unknown = path.with_file_name("2026-08-29T00-00-00-000Z_session-unknown.jsonl");
        fs::copy(fixture("session-unknown-version.jsonl"), &unknown).unwrap();
        assert_eq!(lookup_session(&paths, &unknown), SessionLookup::Unreadable);

        let long = path.with_file_name("2026-08-29T00-00-00-000Z_session-long.jsonl");
        let mut file = File::create(&long).unwrap();
        writeln!(
            file,
            r#"{{"type":"session","version":3,"id":"session-long","cwd":"/workspace"}}"#
        )
        .unwrap();
        file.write_all(&vec![b'x'; MAX_SESSION_LINE_BYTES + 1])
            .unwrap();
        file.write_all(b"\n").unwrap();
        assert_eq!(lookup_session(&paths, &long), SessionLookup::Unreadable);
    }

    #[test]
    fn total_session_read_is_bounded() {
        let root = tempdir().unwrap();
        let (paths, path) = install_fixture(root.path(), "session-codex.jsonl", "session-codex");
        let oversized = path.with_file_name("2026-08-29T00-00-00-000Z_session-huge.jsonl");
        let file = File::create(&oversized).unwrap();
        file.set_len(MAX_SESSION_BYTES + 1).unwrap();
        assert_eq!(
            lookup_session(&paths, &oversized),
            SessionLookup::Unreadable
        );
    }
}
