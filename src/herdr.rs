use crate::model::{ContextUsage, Harness, Provider};
use crate::presentation::MetadataTokens;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Command;

const METADATA_TTL_MS: &str = "86400000";
const MAX_METADATA_TOKENS: usize = 16;
const METADATA_TOKEN_NAMES: [&str; 33] = [
    "quota_state",
    "quota_provider",
    "quota_model",
    "quota_provider_model",
    "quota_summary",
    "quota_context",
    "quota_cache",
    "quota_cache_ttl",
    "quota_5h",
    "quota_5h_label",
    "quota_5h_eta",
    "quota_5h_normal",
    "quota_5h_caution",
    "quota_5h_warning",
    "quota_5h_danger",
    "quota_5h_unknown",
    "quota_week",
    "quota_week_label",
    "quota_week_eta",
    "quota_week_normal",
    "quota_week_caution",
    "quota_week_warning",
    "quota_week_danger",
    "quota_week_unknown",
    "quota_week_inline_label",
    "quota_week_inline_eta",
    "quota_week_inline_normal",
    "quota_week_inline_caution",
    "quota_week_inline_warning",
    "quota_week_inline_danger",
    "quota_week_inline_unknown",
    "quota_topic",
    "quota_error",
];
const OBSOLETE_METADATA_TOKEN_NAMES: [&str; 2] = ["quota_icon", "quota_status"];
const LEGACY_METADATA_TOKEN_NAMES: [&str; 2] = ["quota_badge", "quota_session"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSession {
    pub kind: Option<String>,
    pub value: String,
}

impl AgentSession {
    /// Existing Herdr integrations historically omitted `kind`; keep treating
    /// those values as opaque ids. A path is never exposed through this seam.
    pub fn id(&self) -> Option<&str> {
        self.kind
            .as_deref()
            .is_none_or(|kind| kind == "id")
            .then_some(self.value.as_str())
    }

    pub fn path(&self) -> Option<&str> {
        self.kind
            .as_deref()
            .is_some_and(|kind| kind == "path")
            .then_some(self.value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPane {
    pub pane_id: String,
    pub harness: Harness,
    pub session: Option<AgentSession>,
    pub session_summary: String,
    pub topic: String,
    pub tokens: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentState {
    pub panes: Vec<AgentPane>,
    pub working_providers: Vec<Provider>,
}

#[derive(Debug, Clone)]
pub struct PaneIdentity {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub enum PaneQuotaUpdate {
    Replace(Box<MetadataTokens>),
    Clear,
    Preserve,
}

#[derive(Debug, Clone)]
pub struct PaneTokens {
    pub pane_id: String,
    pub quota: PaneQuotaUpdate,
    pub identity: Option<PaneIdentity>,
    pub context: Option<ContextUsage>,
}

pub fn list_agent_panes() -> Result<Vec<AgentPane>> {
    Ok(list_agent_state()?.panes)
}

/// Read Herdr's agent inventory once and derive both panes and working
/// providers from that same response. The active-turn watcher uses this
/// combined view so one poll does not fan out into one `agent list` call per
/// provider.
pub fn list_agent_state() -> Result<AgentState> {
    let value = list_agent_value()?;
    let mut panes = Vec::new();
    collect_agent_panes(&value, &mut panes);
    panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
    panes.dedup_by(|left, right| left.pane_id == right.pane_id);
    Ok(AgentState {
        panes,
        working_providers: working_providers_from(&value),
    })
}

/// Return whether at least one pane for a provider is currently working.
///
/// This provider-specific helper only asks Herdr for agent metadata; it never
/// reads terminal output. The global watcher uses [`list_agent_state`] so all
/// providers share one inventory call per poll.
pub fn provider_has_working_agent(provider: Provider) -> Result<bool> {
    Ok(list_agent_state()?.working_providers.contains(&provider))
}

fn list_agent_value() -> Result<Value> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(&executable)
        .args(["agent", "list"])
        .output()
        .context("list Herdr agents")?;
    if !output.status.success() {
        anyhow::bail!("Herdr agent list failed with {}", output.status);
    }
    serde_json::from_slice(&output.stdout).context("parse Herdr agent list")
}

/// Pane id and harness of the focused pane.
///
/// `pane.focused` carries no agent in its payload, so this is the only way to
/// learn which pane the user moved to. Herdr answers with the pane id, which
/// is what keeps `focus` scoped to exactly one pane.
pub fn current_focused_pane() -> Result<Option<(String, Harness)>> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(executable)
        .args(["pane", "current"])
        .output()
        .context("read focused Herdr pane")?;
    if !output.status.success() {
        anyhow::bail!("Herdr pane current failed with {}", output.status);
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).context("parse focused Herdr pane")?;
    let pane = value.pointer("/result/pane").unwrap_or(&value);
    let Some(pane_id) = pane.get("pane_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    Ok(pane
        .get("agent")
        .and_then(Value::as_str)
        .and_then(Harness::from_agent_name)
        .map(|harness| (pane_id.to_string(), harness)))
}

// Reading a pane makes Herdr repaint it, which visibly scrolls the agent's
// terminal. Only the pane that fired the event is worth that cost; every other
// pane keeps the topic it last published.
pub fn refresh_pane_topic(pane: &mut AgentPane) {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    if let Some(topic) = read_pane_topic(&executable, pane) {
        pane.topic = topic;
    }
}

fn collect_agent_panes(value: &Value, panes: &mut Vec<AgentPane>) {
    match value {
        Value::Object(map) => {
            let pane_id = map
                .get("pane_id")
                .or_else(|| map.get("paneId"))
                .and_then(Value::as_str);
            let kind = map
                .get("agent")
                .and_then(Value::as_str)
                .or_else(|| map.get("kind").and_then(Value::as_str))
                .or_else(|| {
                    map.get("agent_session")
                        .and_then(Value::as_object)
                        .and_then(|session| session.get("agent"))
                        .and_then(Value::as_str)
                });
            if let (Some(pane_id), Some(kind)) = (pane_id, kind) {
                if let Some(harness) = Harness::from_agent_name(kind) {
                    let tokens: BTreeMap<String, String> = map
                        .get("tokens")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flat_map(|tokens| tokens.iter())
                        .filter_map(|(name, value)| {
                            value
                                .as_str()
                                .map(|value| (name.clone(), value.to_string()))
                        })
                        .collect();
                    let topic = tokens.get("quota_topic").cloned().unwrap_or_default();
                    let session_summary = tokens.get("quota_session").cloned().unwrap_or_default();
                    let session =
                        map.get("agent_session")
                            .and_then(Value::as_object)
                            .and_then(|session| {
                                session.get("value").and_then(Value::as_str).map(|value| {
                                    AgentSession {
                                        kind: session
                                            .get("kind")
                                            .and_then(Value::as_str)
                                            .map(str::to_string),
                                        value: value.to_string(),
                                    }
                                })
                            });
                    panes.push(AgentPane {
                        pane_id: pane_id.to_string(),
                        harness,
                        session,
                        session_summary,
                        // Preserve the last published topic during quota-only
                        // refreshes. Agent events refresh it from pane output.
                        topic,
                        tokens,
                    });
                }
            }
            for child in map.values() {
                collect_agent_panes(child, panes);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_agent_panes(child, panes);
            }
        }
        _ => {}
    }
}

fn working_providers_from(value: &Value) -> Vec<Provider> {
    let mut providers = Vec::new();
    collect_working_providers(value, &mut providers);
    providers.sort_by_key(|provider| {
        Provider::ALL
            .iter()
            .position(|candidate| candidate == provider)
    });
    providers.dedup();
    providers
}

fn collect_working_providers(value: &Value, providers: &mut Vec<Provider>) {
    match value {
        Value::Object(map) => {
            let kind = map
                .get("agent")
                .and_then(Value::as_str)
                .or_else(|| map.get("kind").and_then(Value::as_str))
                .or_else(|| {
                    map.get("agent_session")
                        .and_then(Value::as_object)
                        .and_then(|session| session.get("agent"))
                        .and_then(Value::as_str)
                });
            let status = map
                .get("agent_status")
                .or_else(|| map.get("agentStatus"))
                .or_else(|| map.get("status"))
                .or_else(|| map.get("state"))
                .and_then(Value::as_str);
            if let (Some(kind), Some(status)) = (kind, status) {
                if status.eq_ignore_ascii_case("working") {
                    if let Some(provider) = Harness::billing_for_agent(kind) {
                        providers.push(provider);
                    }
                }
            }
            for child in map.values() {
                collect_working_providers(child, providers);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_working_providers(child, providers);
            }
        }
        _ => {}
    }
}

/// Publish one provider-wide token set to every matching pane.
///
/// Kept as the compatibility entry point for callers that do not need
/// session-specific model labels. The refresh path uses
/// [`publish_pane_tokens`] so same-provider panes can differ.
pub fn publish_tokens(
    panes: &[AgentPane],
    tokens: &[(Provider, MetadataTokens)],
    sequence: u64,
) -> Result<()> {
    let pane_tokens = panes
        .iter()
        .filter_map(|pane| {
            tokens
                .iter()
                .find(|(provider, _)| pane.harness.billing() == Some(*provider))
                .map(|(_, values)| PaneTokens {
                    pane_id: pane.pane_id.clone(),
                    quota: PaneQuotaUpdate::Replace(Box::new(values.clone())),
                    identity: None,
                    context: None,
                })
        })
        .collect::<Vec<_>>();
    publish_pane_tokens(panes, &pane_tokens, sequence)
}

pub fn publish_pane_tokens(
    panes: &[AgentPane],
    tokens: &[PaneTokens],
    sequence: u64,
) -> Result<()> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let mut reported = 0usize;
    let mut failed = Vec::new();
    for pane in panes {
        let Some(pane_tokens) = tokens.iter().find(|tokens| tokens.pane_id == pane.pane_id) else {
            continue;
        };
        let topic = display_topic(pane);
        let mut desired = match &pane_tokens.quota {
            PaneQuotaUpdate::Replace(values) => desired_tokens(values, &topic),
            PaneQuotaUpdate::Clear => desired_cleared_quota(pane),
            PaneQuotaUpdate::Preserve => pane.tokens.clone(),
        };
        if let Some(identity) = &pane_tokens.identity {
            apply_identity(&mut desired, identity);
        }
        if let Some(context) = &pane_tokens.context {
            apply_context(&mut desired, context, sequence / 1_000);
        }
        if metadata_matches(&pane.tokens, &desired) {
            continue;
        }
        // Herdr versions that repaint metadata can snap a terminal viewport
        // back to the bottom. Never mutate pane metadata while the user is
        // reading scrollback; the next refresh after they return catches up.
        if pane_is_scrolled(&executable, &pane.pane_id) {
            continue;
        }
        reported += 1;
        let mut command = Command::new(&executable);
        command
            .args([
                "pane",
                "report-metadata",
                &pane.pane_id,
                "--source",
                "herdr-agent-quota",
            ])
            .args(["--seq", &sequence.to_string()])
            .args(["--ttl-ms", METADATA_TTL_MS]);
        for name in metadata_report_names(pane, &desired) {
            if let Some(value) = desired.get(name) {
                command.args(["--token", &format!("{name}={value}")]);
            } else {
                command.args(["--clear-token", name]);
            }
        }
        let output = command.output().context("report quota metadata to Herdr")?;
        if !output.status.success() {
            failed.push(pane.pane_id.clone());
        }
    }
    // A pane can exit between `agent list` and this report, and the exit event
    // itself triggers a publish. One stale pane id must not stop the panes
    // that are still alive from being updated.
    if reported > 0 && failed.len() == reported {
        anyhow::bail!(
            "Herdr metadata report failed for every pane: {}",
            failed.join(", ")
        );
    }
    Ok(())
}

fn pane_is_scrolled(executable: &std::ffi::OsStr, pane_id: &str) -> bool {
    let Ok(output) = Command::new(executable)
        .args(["pane", "get", pane_id])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|value| {
            value
                .pointer("/result/pane/scroll/offset_from_bottom")
                .and_then(Value::as_u64)
        })
        .is_some_and(|offset| offset > 0)
}

fn desired_tokens(values: &MetadataTokens, topic: &str) -> BTreeMap<String, String> {
    let mut tokens = BTreeMap::from([
        ("quota_provider".to_string(), values.quota_provider.clone()),
        (
            "quota_provider_model".to_string(),
            values.quota_provider_model.clone(),
        ),
    ]);
    insert_optional_token(&mut tokens, "quota_model", &values.quota_model);
    insert_optional_token(&mut tokens, "quota_context", &values.quota_context);
    insert_optional_token(&mut tokens, "quota_cache", &values.quota_cache);
    insert_optional_token(&mut tokens, "quota_cache_ttl", &values.quota_cache_ttl);
    let week_base = week_style_base(&values.quota_5h);
    insert_severity_token(
        &mut tokens,
        "quota_5h",
        &values.quota_5h,
        values.quota_5h_severity,
    );
    insert_severity_token(
        &mut tokens,
        week_base,
        &values.quota_week,
        values.quota_week_severity,
    );
    insert_optional_token(&mut tokens, "quota_topic", topic);
    if let Some(error) = &values.quota_error {
        tokens.insert("quota_error".to_string(), error.clone());
    }
    tokens
}

fn display_topic(pane: &AgentPane) -> String {
    let topic = pane.topic.trim();
    if topic.is_empty() || is_status_line(topic) {
        return truncate_topic(&pane.session_summary);
    }
    truncate_topic(topic)
}

pub(crate) fn plugin_quota_present(tokens: &BTreeMap<String, String>) -> bool {
    METADATA_TOKEN_NAMES
        .into_iter()
        .chain(OBSOLETE_METADATA_TOKEN_NAMES)
        .chain(LEGACY_METADATA_TOKEN_NAMES)
        .filter(|name| *name != "quota_topic")
        .any(|name| tokens.contains_key(name))
}

fn desired_cleared_quota(pane: &AgentPane) -> BTreeMap<String, String> {
    let mut tokens = BTreeMap::new();
    let topic = display_topic(pane);
    if !topic.is_empty() {
        tokens.insert("quota_topic".to_string(), topic);
    }
    tokens
}

fn apply_identity(tokens: &mut BTreeMap<String, String>, identity: &PaneIdentity) {
    tokens.insert("quota_provider".to_string(), identity.provider.clone());
    if identity.model.is_empty() {
        tokens.remove("quota_model");
        tokens.insert(
            "quota_provider_model".to_string(),
            identity.provider.clone(),
        );
    } else {
        tokens.insert("quota_model".to_string(), identity.model.clone());
        tokens.insert(
            "quota_provider_model".to_string(),
            format!("{}/{}", identity.provider, identity.model),
        );
    }
}

fn apply_context(tokens: &mut BTreeMap<String, String>, context: &ContextUsage, now_unix: u64) {
    insert_optional_token(
        tokens,
        "quota_context",
        &crate::presentation::sidebar_context(Some(context)),
    );
    let cache = crate::presentation::sidebar_cache(Some(context));
    if cache.is_empty() {
        tokens.remove("quota_cache");
    } else {
        tokens.insert("quota_cache".to_string(), cache);
    }
    let cache_ttl = crate::presentation::sidebar_cache_ttl(Some(context), now_unix);
    if cache_ttl.is_empty() {
        tokens.remove("quota_cache_ttl");
    } else {
        tokens.insert("quota_cache_ttl".to_string(), cache_ttl);
    }
}

fn metadata_matches(
    current: &BTreeMap<String, String>,
    desired: &BTreeMap<String, String>,
) -> bool {
    METADATA_TOKEN_NAMES
        .into_iter()
        .all(|name| current.get(name) == desired.get(name))
        && OBSOLETE_METADATA_TOKEN_NAMES
            .into_iter()
            .all(|name| !current.contains_key(name))
        && LEGACY_METADATA_TOKEN_NAMES
            .into_iter()
            .all(|name| !current.contains_key(name))
}

fn metadata_report_names(
    pane: &AgentPane,
    desired: &BTreeMap<String, String>,
) -> Vec<&'static str> {
    let mut names = METADATA_TOKEN_NAMES
        .into_iter()
        .filter(|name| desired.contains_key(*name) || pane.tokens.contains_key(*name))
        .collect::<Vec<_>>();
    let cleanup_names = OBSOLETE_METADATA_TOKEN_NAMES
        .into_iter()
        .filter(|name| pane.tokens.contains_key(*name))
        .chain(
            LEGACY_METADATA_TOKEN_NAMES
                .into_iter()
                .filter(|name| pane.tokens.contains_key(*name)),
        )
        .collect::<Vec<_>>();
    if names.len() + cleanup_names.len() <= MAX_METADATA_TOKENS {
        names.extend(cleanup_names);
        return names;
    }

    // Herdr accepts at most sixteen token arguments. Reserve room for stale
    // names first so an upgraded pane can actually clear them; unchanged
    // cosmetic fields can be restored on the next bounded report.
    let active_capacity = MAX_METADATA_TOKENS.saturating_sub(cleanup_names.len());
    for candidate in ["quota_summary", "quota_state", "quota_5h", "quota_week"] {
        while names.len() > active_capacity {
            let Some(index) = names.iter().position(|name| {
                *name == candidate && pane.tokens.get(candidate) == desired.get(candidate)
            }) else {
                break;
            };
            names.remove(index);
        }
    }
    while names.len() > active_capacity {
        let Some(index) = names.iter().position(|name| {
            let must_clear = pane.tokens.contains_key(*name) && !desired.contains_key(*name);
            !must_clear
                && !matches!(
                    *name,
                    "quota_context"
                        | "quota_model"
                        | "quota_provider_model"
                        | "quota_cache"
                        | "quota_cache_ttl"
                        | "quota_provider"
                        | "quota_topic"
                        | "quota_week_inline_normal"
                        | "quota_week_inline_caution"
                        | "quota_week_inline_warning"
                        | "quota_week_inline_danger"
                        | "quota_week_inline_unknown"
                )
        }) else {
            break;
        };
        names.remove(index);
    }
    names.truncate(active_capacity);
    names.extend(cleanup_names);
    names
}

fn week_style_base(quota_5h: &str) -> &'static str {
    // Empty 5h publishes week beside context (`context · 7d`). A present 5h
    // keeps week on the limits row so 5h never shares a line with context.
    if quota_5h.trim().is_empty() {
        "quota_week_inline"
    } else {
        "quota_week"
    }
}

fn insert_severity_token(
    tokens: &mut BTreeMap<String, String>,
    base: &str,
    value: &str,
    severity: Option<crate::model::Severity>,
) {
    if value.trim().is_empty() {
        return;
    }
    let variant = severity_variant(severity);
    tokens.insert(format!("{base}_{variant}"), value.to_string());
}

fn severity_variant(severity: Option<crate::model::Severity>) -> &'static str {
    match severity.unwrap_or(crate::model::Severity::Unknown) {
        crate::model::Severity::Warning => "warning",
        crate::model::Severity::Danger => "danger",
        crate::model::Severity::Normal | crate::model::Severity::Caution => "normal",
        crate::model::Severity::Unknown => "unknown",
    }
}

fn insert_optional_token(tokens: &mut BTreeMap<String, String>, name: &str, value: &str) {
    if !value.trim().is_empty() {
        tokens.insert(name.to_string(), value.to_string());
    }
}

// `recent` rebuilds the pane's wrapped scrollback, which takes seconds and
// repaints the pane: the agent's terminal visibly scrolls, once per read.
// `visible` is the current screen only, costs microseconds, and repaints
// nothing. The prompt is on screen at the moment idle->working fires, which is
// exactly when the topic changes; later in the turn it may have scrolled off,
// and then the caller keeps the topic it already published.
fn topic_read_args(pane_id: &str) -> [&str; 7] {
    [
        "pane", "read", pane_id, "--source", "visible", "--format", "text",
    ]
}

fn read_pane_topic(executable: &std::ffi::OsStr, pane: &AgentPane) -> Option<String> {
    let output = Command::new(executable)
        .args(topic_read_args(&pane.pane_id))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    extract_topic(&text, pane.harness)
}

fn extract_topic(text: &str, harness: Harness) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let cleaned_line = strip_control_chars(line);
        let line = cleaned_line.trim();
        let candidate = prompt_candidate(line, harness)?;
        if candidate.is_empty() || is_status_line(candidate) {
            return None;
        }
        Some(truncate_topic(candidate))
    })
}

fn prompt_candidate(line: &str, harness: Harness) -> Option<&str> {
    let marker = match harness {
        Harness::Claude if line.starts_with('❯') => '❯',
        Harness::Codex if line.starts_with('›') => '›',
        Harness::Grok if line.starts_with('❯') => '❯',
        Harness::Grok | Harness::Agy if line.starts_with('>') => '>',
        _ => return None,
    };
    Some(line.trim_start_matches(marker).trim())
}

fn truncate_topic(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    if characters.len() <= 80 {
        return value.to_string();
    }
    let mut topic: String = characters.into_iter().take(77).collect();
    topic.push('…');
    topic
}

fn strip_control_chars(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || *character == '\t')
        .collect()
}

fn is_status_line(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("accept-edits mode:")
        || lower.starts_with("context ")
        || lower.starts_with("session ")
        || lower.starts_with("auto mode")
        || lower.starts_with("shift+tab")
        || lower == "ask codex to do anything"
        || matches!(
            lower.as_str(),
            "/clear" | "/compact" | "/help" | "/status" | "/usage" | "/model" | "/config"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CacheUsage, ContextUsage, ProviderSnapshot, ResetAt, UsageWindow, WindowKind,
    };
    use serde_json::json;

    #[test]
    fn discovers_canonical_agent_panes_from_nested_json() {
        let value = json!({"result": {"agents": [
            {"pane_id": "w1:p1", "tab_id": "w1:t1", "agent": "codex"},
            {"pane_id": "w1:p2", "tab_id": "w1:t2", "agent_session": {"agent": "claude"}},
            {"pane_id": "w1:p3", "agent": "unknown"},
            {"pane_id": "w1:p4", "agent": "opencode"}
        ], "tabs": [
            {"tab_id": "w1:t1", "label": "Owner"},
            {"tab_id": "w1:t2", "label": "Executor"}
        ]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
        assert_eq!(
            panes,
            vec![
                AgentPane {
                    pane_id: "w1:p1".to_string(),
                    harness: Harness::Codex,
                    session: None,
                    session_summary: String::new(),
                    topic: String::new(),
                    tokens: BTreeMap::new(),
                },
                AgentPane {
                    pane_id: "w1:p2".to_string(),
                    harness: Harness::Claude,
                    session: None,
                    session_summary: String::new(),
                    topic: String::new(),
                    tokens: BTreeMap::new(),
                },
                AgentPane {
                    pane_id: "w1:p4".to_string(),
                    harness: Harness::OpenCode,
                    session: None,
                    session_summary: String::new(),
                    topic: String::new(),
                    tokens: BTreeMap::new(),
                },
            ]
        );
    }

    /// Herdr reports at most 16 metadata tokens per pane. A snapshot that
    /// fills every optional slot must still fit, or the tail is silently
    /// dropped and the sidebar loses whichever rows land last.
    #[test]
    fn a_fully_populated_pane_stays_within_herdrs_sixteen_token_report_cap() {
        const HERDR_TOKEN_REPORT_CAP: usize = 16;
        for provider in [
            Provider::Codex,
            Provider::Grok,
            Provider::Claude,
            Provider::Agy,
            Provider::OpenCodeGo,
        ] {
            let snapshot = ProviderSnapshot::new(
                provider,
                vec![
                    UsageWindow::new(
                        WindowKind::FiveHour,
                        85.0,
                        Some(ResetAt::from_unix_seconds(9_000)),
                    )
                    .unwrap(),
                    UsageWindow::new(
                        WindowKind::Weekly,
                        42.0,
                        Some(ResetAt::from_unix_seconds(600_000)),
                    )
                    .unwrap(),
                    UsageWindow::new(
                        WindowKind::Monthly,
                        10.0,
                        Some(ResetAt::from_unix_seconds(2_000_000)),
                    )
                    .unwrap(),
                ],
                0,
            )
            .with_model(Some("A Very Long Model Name".to_string()))
            .with_context(Some(ContextUsage {
                used_percent: 61.0,
                cache: Some(CacheUsage {
                    fresh_input_tokens: 1_000,
                    read_tokens: 50_000,
                    creation_tokens: 2_000,
                    hit_percent: 96.4,
                    ttl_seconds: Some(3_540),
                    last_activity_unix: None,
                    expires_at_unix: None,
                    session_totals: None,
                    session_id: None,
                    transcript_offset: 0,
                }),
            }));
            let desired = desired_tokens(
                &MetadataTokens::from_snapshot(&snapshot, 0),
                "a topic that is present",
            );
            assert!(
                desired.len() <= HERDR_TOKEN_REPORT_CAP,
                "{provider:?} would report {} tokens: {:?}",
                desired.len(),
                desired.keys().collect::<Vec<_>>()
            );
            // A monthly window must not have leaked in through a weekly token.
            for (name, value) in &desired {
                assert!(
                    !value.contains("30d"),
                    "{provider:?} put a monthly value in {name}"
                );
            }
        }
    }

    #[test]
    fn plugin_quota_presence_ignores_topic_only_tokens() {
        let mut tokens = BTreeMap::new();
        tokens.insert("quota_topic".to_string(), "keep me".to_string());
        assert!(!plugin_quota_present(&tokens));
        tokens.insert("quota_5h".to_string(), "5h 10%".to_string());
        assert!(plugin_quota_present(&tokens));
    }

    #[test]
    fn retains_opencode_pane_session_id() {
        let value = json!({"result": {"agents": [{
            "pane_id": "w1:p9",
            "agent": "opencode",
            "agent_session": {"agent": "opencode", "value": "ses_go"}
        }]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(panes[0].harness, Harness::OpenCode);
        assert_eq!(
            panes[0].session.as_ref().and_then(AgentSession::id),
            Some("ses_go")
        );
    }

    #[test]
    fn carries_path_kind_without_exposing_it_as_an_id() {
        let value = json!({"result": {"agents": [{
            "pane_id": "w1:p9",
            "agent": "pi",
            "agent_session": {
                "agent": "pi",
                "kind": "path",
                "source": "herdr:pi",
                "value": "/tmp/pi/sessions/project/session-pi.jsonl"
            }
        }]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        let session = panes[0].session.as_ref().unwrap();
        assert_eq!(panes[0].harness, Harness::Pi);
        assert_eq!(session.kind.as_deref(), Some("path"));
        assert_eq!(session.id(), None);
        assert_eq!(
            session.path(),
            Some("/tmp/pi/sessions/project/session-pi.jsonl")
        );
    }

    #[test]
    fn id_kind_preserves_every_existing_harness_session() {
        for agent in ["claude", "codex", "grok", "agy", "opencode"] {
            let value = json!({"result": {"agents": [{
                "pane_id": "w1:p1",
                "agent": agent,
                "agent_session": {"kind": "id", "value": "session-id"}
            }]}});
            let mut panes = Vec::new();
            collect_agent_panes(&value, &mut panes);
            assert_eq!(
                panes[0].session.as_ref().and_then(AgentSession::id),
                Some("session-id"),
                "{agent}"
            );
            assert_eq!(
                panes[0].session.as_ref().and_then(AgentSession::path),
                None,
                "{agent}"
            );
        }
    }

    #[test]
    fn unknown_session_kinds_are_not_reinterpreted() {
        for kind in ["PATH", "ID", "uri"] {
            let session = AgentSession {
                kind: Some(kind.to_string()),
                value: "session-value".to_string(),
            };
            assert_eq!(session.id(), None, "{kind}");
            assert_eq!(session.path(), None, "{kind}");
        }
    }

    #[test]
    fn quota_only_discovery_preserves_the_last_published_topic() {
        let value = json!({"result": {"agents": [{
            "pane_id": "w1:p1",
            "agent": "grok",
            "tokens": {"quota_topic": "latest task"}
        }]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(panes[0].topic, "latest task");
    }

    #[test]
    fn discovers_codex_session_id_and_preserves_session_summary() {
        let value = json!({"result": {"agents": [{
            "pane_id": "w1:p1",
            "agent": "codex",
            "agent_session": {"agent": "codex", "value": "thread-1"},
            "tokens": {"quota_session": "previous summary"}
        }]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(
            panes[0].session.as_ref().and_then(AgentSession::id),
            Some("thread-1")
        );
        assert_eq!(panes[0].session_summary, "previous summary");
    }

    #[test]
    fn legacy_metadata_tokens_force_one_bounded_cleanup_report() {
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([(String::from("quota_badge"), String::from("[A]"))]),
        };
        let desired = BTreeMap::from([(String::from("quota_state"), String::from("?"))]);
        assert!(!metadata_matches(&pane.tokens, &desired));
        let names = metadata_report_names(&pane, &desired);
        assert!(names.contains(&"quota_badge"));
        assert!(names.len() <= MAX_METADATA_TOKENS);
    }

    #[test]
    fn weekly_only_inline_week_stays_inside_herdr_metadata_cap() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Grok,
            vec![crate::model::UsageWindow::new(
                crate::model::WindowKind::Weekly,
                30.0,
                Some(crate::model::ResetAt::from_unix_seconds(183_600)),
            )
            .unwrap()],
            0,
        )
        .with_context(Some(
            crate::model::ContextUsage::new(42.0)
                .unwrap()
                .with_cache(Some(
                    crate::model::CacheUsage::from_token_counts(100, 800, 100)
                        .unwrap()
                        .with_ttl_estimate(3_600, 0)
                        .with_session_totals(
                            crate::model::CacheTotals::from_token_counts(100, 800, 100),
                            "session-1",
                            1,
                        ),
                )),
        ));
        let desired = desired_tokens(&MetadataTokens::from_snapshot(&snapshot, 0), "prompt");
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
        };
        let names = metadata_report_names(&pane, &desired);
        assert!(names.len() <= MAX_METADATA_TOKENS);
        assert!(names.contains(&"quota_week_inline_normal"));
        assert!(!names.contains(&"quota_week_normal"));
        assert!(!names.contains(&"quota_5h"));
    }

    #[test]
    fn cache_diagnostics_stay_inside_herdr_metadata_cap() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![
                crate::model::UsageWindow::new(crate::model::WindowKind::FiveHour, 20.0, None)
                    .unwrap(),
                crate::model::UsageWindow::new(crate::model::WindowKind::Weekly, 30.0, None)
                    .unwrap(),
            ],
            0,
        )
        .with_context(Some(
            crate::model::ContextUsage::new(42.0)
                .unwrap()
                .with_cache(Some(
                    crate::model::CacheUsage::from_token_counts(100, 800, 100)
                        .unwrap()
                        .with_ttl_estimate(3_600, 0)
                        .with_session_totals(
                            crate::model::CacheTotals::from_token_counts(100, 800, 100),
                            "session-1",
                            1,
                        ),
                )),
        ));
        let desired = desired_tokens(&MetadataTokens::from_snapshot(&snapshot, 0), "prompt");
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
        };
        let names = metadata_report_names(&pane, &desired);
        assert!(names.len() <= MAX_METADATA_TOKENS);
        assert!(names.contains(&"quota_cache"));
        assert!(names.contains(&"quota_cache_ttl"));
    }

    #[test]
    fn exact_context_without_cache_clears_stale_cache_diagnostics() {
        let mut tokens = BTreeMap::from([
            ("quota_context".to_string(), "context 99%".to_string()),
            ("quota_cache".to_string(), "cache 95.0%".to_string()),
            ("quota_cache_ttl".to_string(), "ttl≈1h".to_string()),
        ]);
        apply_context(&mut tokens, &ContextUsage::new(12.0).unwrap(), 0);
        assert_eq!(
            tokens.get("quota_context").map(String::as_str),
            Some("context 12%")
        );
        assert!(!tokens.contains_key("quota_cache"));
        assert!(!tokens.contains_key("quota_cache_ttl"));
    }

    #[test]
    fn stale_metadata_tokens_are_reported_for_cleanup_with_new_cache_rows() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![
                crate::model::UsageWindow::new(crate::model::WindowKind::FiveHour, 20.0, None)
                    .unwrap(),
                crate::model::UsageWindow::new(crate::model::WindowKind::Weekly, 30.0, None)
                    .unwrap(),
            ],
            0,
        )
        .with_context(Some(
            crate::model::ContextUsage::new(42.0)
                .unwrap()
                .with_cache(Some(
                    crate::model::CacheUsage::from_token_counts(100, 800, 100)
                        .unwrap()
                        .with_ttl_estimate(3_600, 0)
                        .with_session_totals(
                            crate::model::CacheTotals::from_token_counts(100, 800, 100),
                            "session-1",
                            1,
                        ),
                )),
        ));
        let desired = desired_tokens(&MetadataTokens::from_snapshot(&snapshot, 0), "prompt");
        let mut tokens = desired.clone();
        tokens.insert("quota_icon".to_string(), "✦Cl".to_string());
        tokens.insert("quota_status".to_string(), "OK".to_string());
        tokens.insert("quota_badge".to_string(), "[C]".to_string());
        tokens.insert("quota_session".to_string(), "old".to_string());
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens,
        };
        let names = metadata_report_names(&pane, &desired);
        assert!(names.len() <= MAX_METADATA_TOKENS);
        assert!(names.contains(&"quota_cache"));
        assert!(names.contains(&"quota_cache_ttl"));
        assert!(names.contains(&"quota_icon"));
        assert!(names.contains(&"quota_status"));
        assert!(names.contains(&"quota_badge"));
        assert!(names.contains(&"quota_session"));
    }

    #[test]
    fn working_agent_detection_handles_herdr_agent_list_shape() {
        let value = json!({"result": {"agents": [
            {"agent": "claude", "agent_status": "working"},
            {"agent": "codex", "agent_status": "idle"},
            {"agent": "opencode", "agent_status": "working"}
        ]}});
        assert_eq!(working_providers_from(&value), vec![Provider::Claude]);
    }

    #[test]
    fn one_agent_inventory_deduplicates_working_providers() {
        let value = json!({"result": {"agents": [
            {"agent": "codex", "agent_status": "working"},
            {"agent_session": {"agent": "codex"}, "status": "working"},
            {"agent": "claude", "agent_status": "idle"}
        ]}});
        assert_eq!(working_providers_from(&value), vec![Provider::Codex]);
    }

    #[test]
    fn extracts_latest_agy_prompt_instead_of_status_line() {
        let text = "> older\nHello\n> hi\nHello!\n> Accept-edits mode: file edits auto-approved\n";
        assert_eq!(extract_topic(text, Harness::Agy).as_deref(), Some("hi"));
    }

    #[test]
    fn extracts_latest_claude_prompt_and_skips_clear_command() {
        let text = "❯ /clear\n❯ hi\n⏺ Hi! What can I help with?\n❯\n";
        assert_eq!(extract_topic(text, Harness::Claude).as_deref(), Some("hi"));
    }

    #[test]
    fn ignores_codex_default_prompt_placeholder() {
        assert_eq!(
            extract_topic("› Ask Codex to do anything\n", Harness::Codex),
            None
        );
    }

    #[test]
    fn ignores_ai_status_title_as_a_topic() {
        let value = json!({
            "pane_id": "w1:p1",
            "agent": "grok",
            "terminal_title_stripped": "Thinking - L7 Learning Reset"
        });
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(panes[0].topic, "");
    }

    #[test]
    fn claude_placeholder_five_hour_does_not_fold_week_onto_context() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![crate::model::UsageWindow::new(
                crate::model::WindowKind::Weekly,
                31.0,
                Some(crate::model::ResetAt::from_unix_seconds(183_600)),
            )
            .unwrap()],
            0,
        );
        let desired = desired_tokens(&MetadataTokens::from_snapshot(&snapshot, 0), "prompt");
        assert_eq!(
            desired.get("quota_5h_unknown").map(String::as_str),
            Some("5h N/A")
        );
        assert!(!desired.contains_key("quota_5h_label"));
        assert!(desired.contains_key("quota_week_normal"));
        assert!(!desired.contains_key("quota_week_inline_normal"));
        assert!(!desired.contains_key("quota_week_inline_warning"));
        assert!(!desired.contains_key("quota_week_inline_danger"));
    }

    #[test]
    fn empty_five_hour_publishes_week_beside_context() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Codex,
            vec![crate::model::UsageWindow::new(
                crate::model::WindowKind::Weekly,
                31.0,
                Some(crate::model::ResetAt::from_unix_seconds(183_600)),
            )
            .unwrap()],
            0,
        );
        let desired = desired_tokens(&MetadataTokens::from_snapshot(&snapshot, 0), "prompt");
        assert!(!desired.contains_key("quota_5h_normal"));
        assert!(!desired.contains_key("quota_5h_label"));
        assert!(desired.contains_key("quota_week_inline_normal"));
        assert!(!desired.contains_key("quota_week_inline_label"));
        assert!(!desired.contains_key("quota_week_normal"));
    }

    #[test]
    fn present_five_hour_keeps_week_off_the_context_row() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Codex,
            vec![
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::FiveHour,
                    5.0,
                    Some(crate::model::ResetAt::from_unix_seconds(14_820)),
                )
                .unwrap(),
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::Weekly,
                    1.0,
                    Some(crate::model::ResetAt::from_unix_seconds(183_600)),
                )
                .unwrap(),
            ],
            0,
        );
        let desired = desired_tokens(&MetadataTokens::from_snapshot(&snapshot, 0), "prompt");
        assert_eq!(
            desired.get("quota_5h_normal").map(String::as_str),
            Some("5h 95% 4h07m")
        );
        assert_eq!(
            desired.get("quota_week_normal").map(String::as_str),
            Some("7d 99% 2d3h")
        );
        assert!(!desired.contains_key("quota_5h"));
        assert!(!desired.contains_key("quota_5h_label"));
        assert!(!desired.contains_key("quota_5h_eta"));
        assert!(!desired.contains_key("quota_week"));
        assert!(!desired.contains_key("quota_week_inline_normal"));
        assert!(!desired.contains_key("quota_week_inline_warning"));
        assert!(!desired.contains_key("quota_week_inline_danger"));
    }

    #[test]
    fn folding_week_onto_context_clears_limits_week_styles() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Grok,
            vec![crate::model::UsageWindow::new(
                crate::model::WindowKind::Weekly,
                25.0,
                Some(crate::model::ResetAt::from_unix_seconds(183_600)),
            )
            .unwrap()],
            0,
        );
        let desired = desired_tokens(&MetadataTokens::from_snapshot(&snapshot, 0), "prompt");
        let mut tokens = desired.clone();
        tokens.remove("quota_week_inline_normal");
        tokens.insert("quota_week_normal".to_string(), "7d 75% 5d0h".to_string());
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens,
        };
        assert!(!metadata_matches(&pane.tokens, &desired));
        let names = metadata_report_names(&pane, &desired);
        assert!(names.contains(&"quota_week_normal"));
        assert!(names.contains(&"quota_week_inline_normal"));
        assert!(!desired.contains_key("quota_week_normal"));
        assert!(names.len() <= MAX_METADATA_TOKENS);
    }

    #[test]
    fn switching_into_a_five_hour_window_clears_inline_week() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Codex,
            vec![
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::FiveHour,
                    5.0,
                    Some(crate::model::ResetAt::from_unix_seconds(14_820)),
                )
                .unwrap(),
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::Weekly,
                    1.0,
                    Some(crate::model::ResetAt::from_unix_seconds(183_600)),
                )
                .unwrap(),
            ],
            0,
        );
        let desired = desired_tokens(&MetadataTokens::from_snapshot(&snapshot, 0), "prompt");
        let mut tokens = desired.clone();
        tokens.insert("quota_week_inline_normal".to_string(), "7d 99%".to_string());
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            harness: Harness::Codex,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens,
        };
        assert!(!metadata_matches(&pane.tokens, &desired));
        let names = metadata_report_names(&pane, &desired);
        assert!(names.contains(&"quota_week_inline_normal"));
        assert!(names.contains(&"quota_week_normal"));
        assert!(names.len() <= MAX_METADATA_TOKENS);
    }

    #[test]
    fn publishes_exactly_one_styled_variant_for_each_window() {
        let mut tokens = BTreeMap::new();
        insert_severity_token(
            &mut tokens,
            "quota_week",
            "25%",
            Some(crate::model::Severity::Warning),
        );
        assert_eq!(
            tokens.get("quota_week_warning").map(String::as_str),
            Some("25%")
        );
        assert!(!tokens.contains_key("quota_week_normal"));
        assert!(!tokens.contains_key("quota_week_caution"));
        assert!(!tokens.contains_key("quota_week_danger"));
    }

    #[test]
    fn extracts_latest_grok_user_prompt_instead_of_ai_output() {
        let text = "❯ /goal 你在 ti 工作区接手 L7\n先读计划与权威文档，再按七步做 L7 盘点与设计。\n◇ Ran 1 subagent\n计划已读。先冻结坐标并读材料。\n";
        assert_eq!(
            extract_topic(text, Harness::Grok).as_deref(),
            Some("/goal 你在 ti 工作区接手 L7")
        );
    }

    // `recent` and `recent-unwrapped` rebuild the pane's wrapped scrollback,
    // which repaints it: one read, one visible scroll for the user.
    #[test]
    fn topic_reads_never_rebuild_a_pane_scrollback() {
        let args = topic_read_args("w1:p1");
        assert!(args.contains(&"visible"));
        assert!(!args.contains(&"recent"));
        assert!(!args.contains(&"recent-unwrapped"));
    }

    #[test]
    fn truncates_topics_without_splitting_utf8() {
        let topic = truncate_topic(&"你好".repeat(50));
        assert!(topic.ends_with('…'));
        assert!(topic.chars().count() <= 78);
    }
}
