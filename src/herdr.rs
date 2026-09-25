use crate::cache::CacheStore;
use crate::identity::{self, PLUGIN_ID};
use crate::model::{ContextUsage, Harness, Provider};
use crate::presentation::{MetadataTokens, RowStyle, SidebarShape};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

const METADATA_TTL_MS: &str = "86400000";
const MAX_METADATA_TOKENS: usize = 16;
/// Every name [`desired_tokens`] can produce, and nothing else.
///
/// This list is the comparison set for [`metadata_matches`] and the report set
/// for [`metadata_report_names`]. A name that is listed but never produced is
/// not free: it is compared on every refresh and it competes for Herdr's
/// 16-token report budget. Add a name here only together with the field that
/// fills it.
const METADATA_TOKEN_NAMES: [&str; 52] = [
    "quota_group",
    "quota_pad",
    "quota_icon",
    "quota_icon_working",
    "quota_icon_done",
    "quota_provider",
    "quota_model",
    "quota_provider_model",
    "quota_context",
    "quota_context_normal",
    "quota_context_warning",
    "quota_context_danger",
    "quota_cache",
    "quota_cache_ttl",
    "quota_cache_state",
    "quota_5h_normal",
    "quota_5h_warning",
    "quota_5h_danger",
    "quota_5h_unknown",
    "quota_week_normal",
    "quota_week_warning",
    "quota_week_danger",
    "quota_week_unknown",
    "quota_week_inline_normal",
    "quota_week_inline_warning",
    "quota_week_inline_danger",
    "quota_week_inline_unknown",
    "quota_month_normal",
    "quota_month_warning",
    "quota_month_danger",
    "quota_month_unknown",
    "quota_share_5h_normal",
    "quota_share_5h_warning",
    "quota_share_5h_danger",
    "quota_share_5h_unknown",
    "quota_share_week_normal",
    "quota_share_week_warning",
    "quota_share_week_danger",
    "quota_share_week_unknown",
    "quota_share_week_inline_normal",
    "quota_share_week_inline_warning",
    "quota_share_week_inline_danger",
    "quota_share_week_inline_unknown",
    "quota_share_month_normal",
    "quota_share_month_warning",
    "quota_share_month_danger",
    "quota_share_month_unknown",
    "quota_topic",
    "quota_error",
    HEADROOM_TOKEN,
    STACK_TOKEN,
    NEST_GAP_TOKEN,
];
/// Sort key for Herdr's Agent view: remaining quota as a zero-padded percent
/// (`007`), so Herdr's ordering of the token values is also their numeric
/// ordering.
///
/// Published for every pane whose quota is known, whether or not the user
/// chose `--agent-order quota`, because it never renders: no sidebar row
/// references it. Publishing it unconditionally is what makes changing the
/// order a Herdr-side toggle instead of a metadata write to every pane, and it
/// costs no extra writes — the value only moves when a quota token beside it
/// moves anyway.
pub(crate) const HEADROOM_TOKEN: &str = "quota_headroom";
/// Agent-view sort key that keeps same-Space same-vendor panes adjacent:
/// `{group_headroom:03}{harness:02}{role}{own:03}`. Not rendered.
pub(crate) const STACK_TOKEN: &str = "quota_stack";
/// Trailing blank after a pane when the user wants separated agents.
/// Nested vendor children omit it so they stay flush; Herdr `row_gap` is 0.
pub(crate) const NEST_GAP_TOKEN: &str = "quota_nest_gap";
/// Must survive Herdr's token trim: NBSP is whitespace and the row vanishes.
const NEST_GAP_VALUE: &str = "\u{200b}\u{2800}";
/// Visible account-quota window names that repeat for every pane of the same
/// login in one Space. Identity, topic, context, and `$quota_headroom` stay:
/// extra tabs remain in the Agent panel, they just omit the duplicate 5h/7d/30d.
const ACCOUNT_QUOTA_TOKEN_NAMES: [&str; 16] = [
    "quota_5h_normal",
    "quota_5h_warning",
    "quota_5h_danger",
    "quota_5h_unknown",
    "quota_week_normal",
    "quota_week_warning",
    "quota_week_danger",
    "quota_week_unknown",
    "quota_week_inline_normal",
    "quota_week_inline_warning",
    "quota_week_inline_danger",
    "quota_week_inline_unknown",
    "quota_month_normal",
    "quota_month_warning",
    "quota_month_danger",
    "quota_month_unknown",
];
/// The subset of [`METADATA_TOKEN_NAMES`] whose value comes from the cached
/// quota windows and nothing else. [`quota_rows_have_drifted`] compares these,
/// so a name added here must be one a snapshot alone can render.
const QUOTA_WINDOW_TOKEN_NAMES: [&str; 17] = [
    "quota_5h_normal",
    "quota_5h_warning",
    "quota_5h_danger",
    "quota_5h_unknown",
    "quota_week_normal",
    "quota_week_warning",
    "quota_week_danger",
    "quota_week_unknown",
    "quota_week_inline_normal",
    "quota_week_inline_warning",
    "quota_week_inline_danger",
    "quota_week_inline_unknown",
    "quota_month_normal",
    "quota_month_warning",
    "quota_month_danger",
    "quota_month_unknown",
    HEADROOM_TOKEN,
];
/// Names a pane may still carry from an older build of this plugin. They are
/// never produced again, so a report clears them until the pane is clean.
const OBSOLETE_METADATA_TOKEN_NAMES: [&str; 14] = [
    "quota_state",
    "quota_status",
    "quota_summary",
    "quota_5h",
    "quota_5h_label",
    "quota_5h_percent",
    "quota_5h_eta",
    "quota_5h_caution",
    "quota_week",
    "quota_week_label",
    "quota_week_percent",
    "quota_week_eta",
    "quota_week_caution",
    "quota_week_inline_caution",
];
const LEGACY_METADATA_TOKEN_NAMES: [&str; 4] = [
    "quota_badge",
    "quota_session",
    "quota_week_inline_label",
    "quota_week_inline_eta",
];
/// The names the context row can be published into, in the order a report
/// clears them. Herdr fixes a token's colour by name, so the only way to
/// colour the context row is to publish it into a name whose row template
/// already carries that colour — which is why `gauges` needs the three
/// severity variants and `packed`/`stacked` keep the plain one.
///
/// Exactly one is ever filled. Publishing a second would draw two context
/// rows in the same pane.
const CONTEXT_TOKEN_NAMES: [&str; 4] = [
    "quota_context",
    "quota_context_normal",
    "quota_context_warning",
    "quota_context_danger",
];
/// Values that must reach the pane in the *same* report that changed them,
/// even when the budget is tight: the identity, the live diagnostics, and the
/// inline week variants, whose styling flips as soon as a 5h window appears.
const ROWS_THAT_MUST_NOT_LAG: [&str; 19] = [
    "quota_group",
    "quota_icon",
    "quota_provider",
    "quota_model",
    "quota_provider_model",
    "quota_topic",
    "quota_context",
    "quota_context_normal",
    "quota_context_warning",
    "quota_context_danger",
    "quota_cache",
    "quota_cache_ttl",
    "quota_cache_state",
    "quota_week_inline_normal",
    "quota_week_inline_warning",
    "quota_week_inline_danger",
    "quota_week_inline_unknown",
    STACK_TOKEN,
    NEST_GAP_TOKEN,
];

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

/// Herdr's effective pane status from `agent_status`.
///
/// The CLI list maps idle+unseen to `done`, but same-tab completions are
/// often already `idle` on the server while the TUI ring is still teal.
/// Brand-icon colour therefore uses this enum after `refresh` has applied
/// the plugin's own unseen set — never a second `state_icon` ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentStatus {
    #[default]
    Idle,
    Working,
    Done,
    Blocked,
    Unknown,
}

impl AgentStatus {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "working" => Self::Working,
            "done" => Self::Done,
            "blocked" => Self::Blocked,
            "unknown" => Self::Unknown,
            _ => Self::Idle,
        }
    }

    pub fn is_working(self) -> bool {
        matches!(self, Self::Working)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPane {
    pub pane_id: String,
    pub workspace_id: String,
    pub cwd: String,
    pub title: String,
    pub harness: Harness,
    pub session: Option<AgentSession>,
    pub session_summary: String,
    pub topic: String,
    pub tokens: BTreeMap<String, String>,
    /// From Herdr `agent_status`. Drives brand-icon colour and the watch pulse.
    pub status: AgentStatus,
    /// Herdr `focused` describes the current pane, not whether a completion
    /// was acknowledged by a later focus event.
    pub focused: bool,
}

impl AgentPane {
    pub fn working(&self) -> bool {
        self.status.is_working()
    }

    /// Status the brand icon should mirror.
    ///
    /// Working is yellow, unseen completion is teal, acknowledged is white.
    /// Callers fold the plugin unseen-set into `status` before publish. Do
    /// not treat a leftover `$quota_icon_done` token as unseen: a concurrent
    /// inventory read after mark-seen still carries that token and would
    /// paint teal back on.
    pub fn icon_status(&self) -> AgentStatus {
        if self.status.is_working() {
            return AgentStatus::Working;
        }
        if self.status == AgentStatus::Done {
            return AgentStatus::Done;
        }
        self.status
    }

    pub fn icon_needs_update(&self) -> bool {
        // Old installs published colour twins; a leftover name must be
        // cleared even after the layout moved colour onto `$quota_icon`.
        if self.tokens.contains_key("quota_icon_working")
            || self.tokens.contains_key("quota_icon_done")
        {
            return true;
        }
        let value = self
            .tokens
            .get("quota_icon")
            .map(String::as_str)
            .unwrap_or("");
        let working = value.contains(crate::icons::WORKING_TAG);
        let done = value.contains(crate::icons::DONE_TAG);
        match self.icon_status() {
            AgentStatus::Working => !working || done,
            AgentStatus::Done => !done,
            _ => working || done || value.is_empty(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentState {
    pub panes: Vec<AgentPane>,
    pub working_providers: Vec<Provider>,
    pub working_pane_ids: Vec<String>,
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
    /// Account-level 5h/7d/30d is shared across panes of one vendor in a Space.
    /// Only one pane in that group publishes those window rows; the others keep
    /// identity, topic, and context and stay visible in the Agent panel.
    pub show_account_quota: bool,
}

/// Show one Herdr notification.
///
/// Failure is reported to the caller but is never worth aborting a publish
/// for: a missed toast costs the user nothing that the sidebar does not
/// already show.
pub fn notify(title: &str, body: &str) -> Result<()> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(&executable)
        .args(["notification", "show", title, "--body", body])
        .args(["--sound", "request"])
        .output()
        .context("show Herdr notification")?;
    if !output.status.success() {
        anyhow::bail!("Herdr notification failed with {}", output.status);
    }
    Ok(())
}

/// A socket request must not outlive the event hook that sent it. Herdr
/// answers these in microseconds; anything near this is a hung server, and a
/// sidebar sort is never worth blocking a turn for.
const SOCKET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Ask Herdr to order its Agent panel by space, then least quota left.
///
/// Herdr keeps one Agent view and this replaces it. The default agent order
/// is `quota`, so configure and startup both call this unless the user
/// chose `default`. The view does not survive a server restart, which is
/// why the startup hook re-applies it.
///
/// `workspace_order` keeps each Space contiguous — the same grouping Herdr's
/// own spaces sort uses — so quota ranking never scatters one project's
/// agents across the panel. Inside a space, `quota_headroom` ranks tightest
/// first.
pub fn set_quota_agent_view() -> Result<()> {
    socket_request(&serde_json::json!({
        "id": "agent-quota:view-set",
        "method": "agent.view.set",
        "params": {
            "source": identity::agent_view_source(),
            "label": crate::cli::AgentOrder::LABEL,
            "sort": [
                {"field": "workspace_order", "order": "asc"},
                {"field": {"token": STACK_TOKEN}, "order": "asc"},
                {"field": {"token": HEADROOM_TOKEN}, "order": "asc"},
            ],
        },
    }))
    .map(|_| ())
}

/// Give the Agent panel back to Herdr's own ordering.
///
/// Scoped to this plugin's source: a view someone else owns must survive.
pub fn clear_quota_agent_view() -> Result<()> {
    socket_request(&serde_json::json!({
        "id": "agent-quota:view-clear",
        "method": "agent.view.clear",
        "params": {"source": identity::agent_view_source()},
    }))
    .map(|_| ())
}

/// One request, one reply, one connection.
///
/// `agent.view.*` has no CLI subcommand in Herdr 0.8, so this is the only
/// place the plugin speaks the raw socket protocol. Nothing here subscribes,
/// so no stream is ever held open — the replay and focus-storm problems that
/// come with `events.subscribe` do not apply.
///
/// Outside Herdr there is no socket and this is a no-op, exactly like
/// [`crate::prefs::write`], so a direct CLI run still works.
fn socket_request(payload: &Value) -> Result<Option<Value>> {
    use std::io::{BufRead, BufReader, Write};

    let Some(path) = std::env::var_os("HERDR_SOCKET_PATH") else {
        return Ok(None);
    };
    let stream = std::os::unix::net::UnixStream::connect(&path)
        .with_context(|| format!("connect to Herdr at {}", path.to_string_lossy()))?;
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    let mut writer = &stream;
    writeln!(writer, "{payload}").context("send Herdr socket request")?;
    writer.flush().context("flush Herdr socket request")?;
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .context("read Herdr socket reply")?;
    let reply: Value = serde_json::from_str(&line).context("parse Herdr socket reply")?;
    if let Some(error) = reply.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Herdr rejected the request");
        anyhow::bail!("{message}");
    }
    Ok(Some(reply))
}

pub fn list_agent_panes() -> Result<Vec<AgentPane>> {
    Ok(list_agent_state()?.panes)
}

/// Read Herdr's agent inventory once and derive both panes and working
/// providers from that same response. The active-turn watcher uses this
/// combined view so one poll does not fan out into one `agent list` call per
/// provider.
pub fn list_agent_state() -> Result<AgentState> {
    Ok(agent_state_from(
        &list_agent_value()?,
        attach_missing_sessions,
    ))
}

/// Every inventory consumer (watch, focus, sibling publish) sees the same
/// recovered sessions. A pane left without one renders the account-level
/// model, which belongs to whichever rollout Codex wrote last.
fn agent_state_from(value: &Value, attach: impl FnOnce(&mut [AgentPane])) -> AgentState {
    let mut panes = Vec::new();
    collect_agent_panes(value, &mut panes);
    // Preserve Herdr's inventory order: under the default grouped view this is
    // the Agent panel's draw order. Lexical pane ids are not layout order
    // (for example, p10 sorts before p7).
    let mut seen = BTreeSet::new();
    panes.retain(|pane| seen.insert(pane.pane_id.clone()));
    attach(&mut panes);
    let mut working_pane_ids = Vec::new();
    collect_working_providers(value, &mut Vec::new(), &mut working_pane_ids);
    working_pane_ids.sort();
    working_pane_ids.dedup();
    AgentState {
        panes,
        working_providers: working_providers_from(value),
        working_pane_ids,
    }
}

/// One pane from a single inventory read, for the event and focus paths.
/// Only that pane's Muse session is resolved, so an event on another agent
/// never walks Muse's process and session state.
pub fn find_agent_pane(pane_id: &str) -> Result<Option<AgentPane>> {
    let value = list_agent_value()?;
    let mut panes = Vec::new();
    collect_agent_panes(&value, &mut panes);
    let Some(pane) = panes.into_iter().find(|pane| pane.pane_id == pane_id) else {
        return Ok(None);
    };
    let mut panes = [pane];
    attach_missing_sessions(&mut panes);
    let [pane] = panes;
    Ok(Some(pane))
}

/// Return only the named agents from one inventory read. A focus change must
/// never acknowledge an unrelated green pane in the same tab.
pub fn find_agent_icon_panes(pane_ids: &[&str]) -> Result<Vec<AgentPane>> {
    let value = list_agent_value()?;
    let mut panes = Vec::new();
    collect_agent_panes(&value, &mut panes);
    panes.retain(|pane| pane_ids.contains(&pane.pane_id.as_str()));
    panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
    panes.dedup_by(|left, right| left.pane_id == right.pane_id);
    Ok(panes)
}

fn attach_missing_sessions(panes: &mut [AgentPane]) {
    attach_muse_sessions(panes);
    attach_codex_sessions(panes);
}

/// Herdr has no Muse session integration, so a Muse pane arrives without a
/// session. Resolve it from Muse's own session lock; a session Herdr does
/// report is always kept as-is.
fn attach_muse_sessions(panes: &mut [AgentPane]) {
    attach_muse_sessions_with(panes, crate::providers::muse::session_ids_for_panes);
}

/// Codex hooks normally report a session id. A wrapper can disable those
/// hooks while leaving Herdr's foreground cwd intact, so use the rollout's
/// exact session_meta cwd only when one missing pane and one rollout agree.
fn attach_codex_sessions(panes: &mut [AgentPane]) {
    if !crate::cli::AgentSelection::from_args_or_env(&[]).contains(&Harness::Codex) {
        return;
    }
    let mut started = BTreeMap::new();
    for pane in panes
        .iter_mut()
        .filter(|pane| pane.harness == Harness::Codex && pane.session.is_none())
    {
        let Some(process) = codex_foreground_process(&pane.pane_id) else {
            continue;
        };
        if let Some(session_id) = codex_resume_session_id(&process) {
            pane.session = Some(AgentSession {
                kind: Some("id".to_string()),
                value: session_id,
            });
        } else if let Some(time) = codex_process_started_at(&process) {
            started.insert(pane.pane_id.clone(), time);
        }
    }
    attach_codex_sessions_with(
        panes,
        |pane_id| started.get(pane_id).copied(),
        crate::providers::codex::session_ids_for_panes,
    );
}

fn codex_foreground_process(pane_id: &str) -> Option<Value> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(executable)
        .args(["pane", "process-info", "--pane", pane_id])
        .output()
        .ok()?;
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    value
        .pointer("/result/process_info/foreground_processes")?
        .as_array()?
        .iter()
        .find(|process| {
            process
                .get("argv")
                .and_then(Value::as_array)
                .and_then(|argv| argv.first())
                .and_then(Value::as_str)
                .is_some_and(|argv0| argv0 == "codex" || argv0.ends_with("/codex"))
        })
        .cloned()
}

/// A restored pane exposes its exact session in `codex resume <id>` even when
/// Herdr's session hook has not reported it after a reboot.
fn codex_resume_session_id(process: &Value) -> Option<String> {
    let argv = process.get("argv")?.as_array()?;
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next().and_then(Value::as_str) {
        match arg {
            "resume" => {
                while let Some(candidate) = args.next().and_then(Value::as_str) {
                    if codex_session_uuid(candidate) {
                        return Some(candidate.to_string());
                    }
                    match candidate {
                        "--last" | "--all" | "--include-non-interactive" => {}
                        flag if codex_cli_option_takes_value(flag) => {
                            args.next()?.as_str()?;
                        }
                        flag if codex_cli_inline_value(flag) => {}
                        // Unknown post-resume flags fail closed. Treating a
                        // following UUID as the session could bind a pane to a
                        // future option's value instead of the resume target.
                        flag if flag.starts_with('-') => return None,
                        _ => return None,
                    }
                }
                return None;
            }
            flag if codex_cli_option_takes_value(flag) => {
                args.next()?.as_str()?;
            }
            flag if codex_cli_inline_value(flag) || flag.starts_with('-') => {}
            _ => return None,
        }
    }
    None
}

fn codex_session_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn codex_cli_option_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "-m" | "--model"
            | "-c"
            | "--config"
            | "-p"
            | "--profile"
            | "-s"
            | "--sandbox"
            | "-a"
            | "--ask-for-approval"
            | "-C"
            | "--cd"
            | "-i"
            | "--image"
            | "--add-dir"
            | "--enable"
            | "--disable"
            | "--remote"
            | "--remote-auth-token-env"
            | "--local-provider"
    )
}

fn codex_cli_inline_value(flag: &str) -> bool {
    flag.split_once('=')
        .is_some_and(|(name, _)| codex_cli_option_takes_value(name))
}

fn attach_codex_sessions_with(
    panes: &mut [AgentPane],
    started_at: impl Fn(&str) -> Option<u64>,
    resolve: impl FnOnce(&[(String, String, u64)]) -> BTreeMap<String, String>,
) {
    let missing = panes
        .iter()
        .filter(|pane| pane.harness == Harness::Codex && pane.session.is_none())
        .collect::<Vec<_>>();
    // Callers drop unselected harnesses after the inventory read; a partial
    // install must not inspect the processes of an agent it does not track.
    if missing.is_empty()
        || !crate::cli::AgentSelection::from_args_or_env(&[]).contains(&Harness::Codex)
    {
        return;
    }
    let candidates = missing
        .into_iter()
        .filter_map(|pane| {
            started_at(&pane.pane_id)
                .map(|started_at| (pane.pane_id.clone(), pane.cwd.clone(), started_at))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return;
    }
    let resolved = resolve(&candidates);
    for pane in panes {
        if pane.harness != Harness::Codex || pane.session.is_some() {
            continue;
        }
        if let Some(session_id) = resolved.get(&pane.pane_id) {
            pane.session = Some(AgentSession {
                kind: Some("id".to_string()),
                value: session_id.clone(),
            });
        }
    }
}

fn codex_process_started_at(process: &Value) -> Option<u64> {
    let pid = process.get("pid")?.as_u64()?.to_string();
    let elapsed = Command::new("ps")
        .args(["-p", &pid, "-o", "etime="])
        .output()
        .ok()?;
    let seconds = parse_ps_elapsed(&String::from_utf8_lossy(&elapsed.stdout))?;
    Some(CacheStore::now_unix().saturating_sub(seconds))
}

fn parse_ps_elapsed(value: &str) -> Option<u64> {
    let value = value.trim();
    let (days, clock) = if let Some((days, clock)) = value.split_once('-') {
        (days.parse::<u64>().ok()?, clock)
    } else {
        (0, value)
    };
    let mut seconds = 0_u64;
    for part in clock.split(':') {
        seconds = seconds
            .checked_mul(60)?
            .checked_add(part.parse::<u64>().ok()?)?;
    }
    (clock.split(':').count() >= 2).then_some(days * 86_400 + seconds)
}

fn attach_muse_sessions_with(
    panes: &mut [AgentPane],
    resolve: impl FnOnce(&[String]) -> BTreeMap<String, String>,
) {
    let missing = panes
        .iter()
        .filter(|pane| pane.harness == Harness::Muse && pane.session.is_none())
        .map(|pane| pane.pane_id.clone())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return;
    }
    let resolved = resolve(&missing);
    for pane in panes {
        if pane.harness != Harness::Muse || pane.session.is_some() {
            continue;
        }
        if let Some(session_id) = resolved.get(&pane.pane_id) {
            pane.session = Some(AgentSession {
                kind: Some("id".to_string()),
                value: session_id.clone(),
            });
        }
    }
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

/// Resolve a workspace/tab focus event to that tab's focused pane. Ignore a
/// delayed event once the session has focused somewhere else.
pub fn focused_pane_in_snapshot(
    workspace_id: Option<&str>,
    tab_id: Option<&str>,
) -> Result<Option<String>> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(executable)
        .args(["api", "snapshot"])
        .output()
        .context("read Herdr focus snapshot")?;
    if !output.status.success() {
        anyhow::bail!("Herdr api snapshot failed with {}", output.status);
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).context("parse Herdr focus snapshot")?;
    let snapshot = value.pointer("/result/snapshot").unwrap_or(&value);
    if workspace_id
        .is_some_and(|id| snapshot.get("focused_workspace_id").and_then(Value::as_str) != Some(id))
        || tab_id
            .is_some_and(|id| snapshot.get("focused_tab_id").and_then(Value::as_str) != Some(id))
    {
        return Ok(None);
    }
    if workspace_id.is_none() && tab_id.is_none() {
        return Ok(snapshot
            .get("focused_pane_id")
            .and_then(Value::as_str)
            .map(str::to_owned));
    }
    let tab_id = tab_id.or_else(|| {
        snapshot
            .get("workspaces")?
            .as_array()?
            .iter()
            .find(|workspace| {
                workspace.get("workspace_id").and_then(Value::as_str) == workspace_id
            })?
            .get("active_tab_id")?
            .as_str()
    });
    let Some(tab_id) = tab_id else {
        return Ok(None);
    };
    Ok(snapshot
        .get("layouts")
        .and_then(Value::as_array)
        .and_then(|layouts| {
            layouts
                .iter()
                .find(|layout| layout.get("tab_id").and_then(Value::as_str) == Some(tab_id))
        })
        .and_then(|layout| layout.get("focused_pane_id"))
        .and_then(Value::as_str)
        .map(str::to_owned))
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
                    let workspace_id = map
                        .get("workspace_id")
                        .or_else(|| map.get("workspaceId"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| workspace_id_from_pane_id(pane_id))
                        .unwrap_or_default();
                    let status = map
                        .get("agent_status")
                        .or_else(|| map.get("agentStatus"))
                        .or_else(|| map.get("status"))
                        .or_else(|| map.get("state"))
                        .and_then(Value::as_str)
                        .map(AgentStatus::parse)
                        .unwrap_or_default();
                    let focused = map.get("focused").and_then(Value::as_bool).unwrap_or(false);
                    // Codex rollouts record the native foreground process's
                    // cwd. Other collectors keep the pane cwd contract they
                    // already use.
                    let cwd = (harness == Harness::Codex)
                        .then(|| map.get("foreground_cwd"))
                        .flatten()
                        .or_else(|| map.get("cwd"))
                        .or_else(|| map.get("foreground_cwd"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let title = map
                        .get("terminal_title_stripped")
                        .or_else(|| map.get("terminal_title"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    panes.push(AgentPane {
                        pane_id: pane_id.to_string(),
                        workspace_id,
                        cwd,
                        title,
                        harness,
                        session,
                        session_summary,
                        // Preserve the last published topic during quota-only
                        // refreshes. Agent events refresh it from pane output.
                        topic,
                        tokens,
                        status,
                        focused,
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
    collect_working_providers(value, &mut providers, &mut Vec::new());
    providers.sort_by_key(|provider| {
        Provider::ALL
            .iter()
            .position(|candidate| candidate == provider)
    });
    providers.dedup();
    providers
}

fn collect_working_providers(
    value: &Value,
    providers: &mut Vec<Provider>,
    pane_ids: &mut Vec<String>,
) {
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
                    if Harness::from_agent_name(kind).is_some() {
                        if let Some(pane_id) = map
                            .get("pane_id")
                            .or_else(|| map.get("paneId"))
                            .and_then(Value::as_str)
                        {
                            pane_ids.push(pane_id.to_string());
                        }
                    }
                    if let Some(provider) = Harness::billing_for_agent(kind) {
                        providers.push(provider);
                    }
                }
            }
            for child in map.values() {
                collect_working_providers(child, providers, pane_ids);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_working_providers(child, providers, pane_ids);
            }
        }
        _ => {}
    }
}

pub fn publish_pane_tokens(
    panes: &[AgentPane],
    tokens: &[PaneTokens],
    sequence: u64,
    row: RowStyle,
) -> Result<()> {
    publish_pane_tokens_inner(panes, tokens, sequence, row, false)
}

/// Watcher refreshes may need to clear a stale icon while its pane is scrolled.
/// The inner publisher still sends only the icon twins in that case; quota and
/// topic writes remain deferred until the pane is visible.
pub fn publish_pane_tokens_with_scrolled_icons(
    panes: &[AgentPane],
    tokens: &[PaneTokens],
    sequence: u64,
    row: RowStyle,
) -> Result<()> {
    publish_pane_tokens_inner(panes, tokens, sequence, row, true)
}

/// Sidebar icon colour must update on focus even if the terminal is scrolled —
/// the scroll guard exists to protect reading scrollback during quota refreshes,
/// not to leave a stale teal glyph after mark-seen.
pub fn publish_status_icons(
    panes: &[AgentPane],
    tokens: &[PaneTokens],
    sequence: u64,
    row: RowStyle,
) -> Result<()> {
    publish_pane_tokens_inner(panes, tokens, sequence, row, true)
}

/// Focus and watcher reconciliation change only the three icon colour tokens.
/// They must not turn an icon acknowledgement into a quota or group refresh.
pub fn publish_icon_tokens(panes: &[AgentPane], sequence: u64) -> Result<()> {
    if panes.is_empty() {
        return Ok(());
    }
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let inventory = match list_agent_panes() {
        Ok(all) if !all.is_empty() => all,
        _ => panes.to_vec(),
    };
    let nesting = vendor_nesting(&inventory, panes, &[]);
    let group_heads = group_head_pane_ids(
        &inventory,
        panes,
        &[],
        group_head_uses_quota_order(),
        &nesting.stack,
        &BTreeSet::new(),
    );
    let wide = !identity_is_narrow(publish_content_width());
    let mut reported = 0;
    let mut failed = Vec::new();
    for pane in panes {
        let mut desired = pane.tokens.clone();
        let role = vendor_row_for(wide, &nesting, &pane.pane_id);
        apply_group_and_icon(
            &mut desired,
            pane,
            &group_heads,
            &BTreeMap::new(),
            role,
            Some(vendor_icon_status(&nesting, pane, role)),
        );
        if icon_tokens_match(&pane.tokens, &desired) {
            continue;
        }
        reported += 1;
        if !report_icon_metadata(&executable, pane, &desired, sequence)? {
            failed.push(pane.pane_id.clone());
        }
    }
    if reported > 0 && failed.len() == reported {
        anyhow::bail!(
            "Herdr icon report failed for every pane: {}",
            failed.join(", ")
        );
    }
    Ok(())
}

fn publish_pane_tokens_inner(
    panes: &[AgentPane],
    tokens: &[PaneTokens],
    sequence: u64,
    row: RowStyle,
    allow_while_scrolled: bool,
) -> Result<()> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let workspace_labels = list_workspace_labels().unwrap_or_default();
    // Event/focus publish one pane. Head selection and sibling clears need the
    // full Space membership — otherwise a lone pane preserves a stale
    // `$quota_group` and the Space name prints twice (radar writes `group:null`
    // on every non-head every frame).
    let inventory = match list_agent_panes() {
        Ok(all) if !all.is_empty() => all,
        _ => panes.to_vec(),
    };
    let nesting = vendor_nesting(&inventory, panes, tokens);
    let group_heads = group_head_pane_ids(
        &inventory,
        panes,
        tokens,
        group_head_uses_quota_order(),
        &nesting.stack,
        &BTreeSet::new(),
    );
    let wide = !identity_is_narrow(row.shape.content_width);
    let mut reported = 0usize;
    let mut failed = Vec::new();
    for pane in panes {
        let Some(pane_tokens) = tokens.iter().find(|tokens| tokens.pane_id == pane.pane_id) else {
            continue;
        };
        let topic = display_topic(pane);
        let mut desired = match &pane_tokens.quota {
            PaneQuotaUpdate::Replace(values) => desired_tokens(values, &topic, row.shape),
            PaneQuotaUpdate::Clear => desired_cleared_quota(pane),
            PaneQuotaUpdate::Preserve => pane.tokens.clone(),
        };
        let role = vendor_row_for(wide, &nesting, &pane.pane_id);
        // A Codex helper is a separately steerable seat even when it shares
        // account quota with the workspace head. Keep its own identity line.
        let codex_child = role == VendorRow::Child && pane.harness == Harness::Codex;
        if let Some(identity) = &pane_tokens.identity {
            apply_identity(
                &mut desired,
                identity,
                row.shape.content_width,
                if codex_child { VendorRow::Head } else { role },
            );
        } else if codex_child {
            unindent_token(&mut desired, "quota_model");
            desired.insert("quota_provider".to_string(), "Codex".to_string());
            apply_nested_head_from_tokens(&mut desired);
        } else if role == VendorRow::Child {
            apply_nested_child_from_tokens(&mut desired);
        } else if role == VendorRow::Head {
            apply_nested_head_from_tokens(&mut desired);
        }
        if let Some(context) = &pane_tokens.context {
            apply_context(&mut desired, context, sequence / 1_000, row);
        }
        fold_cache_row(&mut desired, row);
        match role {
            VendorRow::Head => {
                // Share-window rows exist only in Gauges. Packed/Stacked would
                // promote 5h/7d/30d onto names those layouts never render.
                if row.shape.layout == crate::cli::SidebarLayout::Gauges {
                    promote_shared_quota(&mut desired);
                }
                strip_vendor_head_session(&mut desired);
            }
            VendorRow::Child => {
                strip_account_quota_tokens(&mut desired);
                strip_vendor_child_extras(&mut desired);
            }
            VendorRow::Flat => {
                if !pane_tokens.show_account_quota {
                    strip_account_quota_tokens(&mut desired);
                }
            }
        }
        strip_flat_gauge_model_if_unused(&mut desired, row, role);
        apply_group_and_icon(
            &mut desired,
            pane,
            &group_heads,
            &workspace_labels,
            if codex_child { VendorRow::Flat } else { role },
            Some(vendor_icon_status(&nesting, pane, role)),
        );
        apply_pack_gap(
            &mut desired,
            role,
            &nesting,
            &pane.pane_id,
            pack_gap_enabled(),
        );
        if let Some(key) = nesting.stack.get(&pane.pane_id) {
            desired.insert(STACK_TOKEN.to_string(), key.clone());
        }
        if metadata_matches(&pane.tokens, &desired) {
            continue;
        }
        // Herdr versions that repaint metadata can snap a terminal viewport
        // back to the bottom. Never mutate pane metadata while the user is
        // reading scrollback; the next refresh after they return catches up.
        // Icon-only sync may proceed while scrolled — see publish_status_icons.
        if pane_is_scrolled(&executable, &pane.pane_id) {
            if !allow_while_scrolled || icon_tokens_match(&pane.tokens, &desired) {
                continue;
            }
            reported += 1;
            if !report_icon_metadata(&executable, pane, &desired, sequence)? {
                failed.push(pane.pane_id.clone());
            }
            continue;
        }
        reported += 1;
        if !report_pane_metadata(&executable, pane, &desired, sequence)? {
            failed.push(pane.pane_id.clone());
        }
    }
    reported += sync_sibling_group_headers(
        &executable,
        &inventory,
        panes,
        &group_heads,
        &workspace_labels,
        sequence,
        &mut failed,
    )?;
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

fn icon_tokens_match(
    current: &BTreeMap<String, String>,
    desired: &BTreeMap<String, String>,
) -> bool {
    ICON_TOKEN_NAMES
        .into_iter()
        .all(|name| current.get(name) == desired.get(name))
}

fn report_icon_metadata(
    executable: &std::ffi::OsStr,
    pane: &AgentPane,
    desired: &BTreeMap<String, String>,
    sequence: u64,
) -> Result<bool> {
    let mut command = Command::new(executable);
    command
        .args([
            "pane",
            "report-metadata",
            &pane.pane_id,
            "--source",
            PLUGIN_ID,
        ])
        .args(["--seq", &sequence.to_string(), "--ttl-ms", METADATA_TTL_MS]);
    for name in ICON_TOKEN_NAMES {
        if let Some(value) = desired.get(name) {
            command.args(["--token", &format!("{name}={value}")]);
        } else {
            command.args(["--clear-token", name]);
        }
    }
    let output = command.output().context("report icon metadata to Herdr")?;
    Ok(output.status.success())
}

fn report_pane_metadata(
    executable: &std::ffi::OsStr,
    pane: &AgentPane,
    desired: &BTreeMap<String, String>,
    sequence: u64,
) -> Result<bool> {
    let mut command = Command::new(executable);
    command
        .args([
            "pane",
            "report-metadata",
            &pane.pane_id,
            "--source",
            PLUGIN_ID,
        ])
        .args(["--seq", &sequence.to_string()])
        .args(["--ttl-ms", METADATA_TTL_MS]);
    for name in metadata_report_names(pane, desired) {
        if let Some(value) = desired.get(name) {
            command.args(["--token", &format!("{name}={value}")]);
        } else {
            command.args(["--clear-token", name]);
        }
    }
    let output = command.output().context("report quota metadata to Herdr")?;
    Ok(output.status.success())
}

/// Clear or set `$quota_group` on siblings in the same Space that this pass
/// did not otherwise touch. Without this, a one-pane event leaves the old
/// head's header in place after headroom moves the title to another pane.
fn sync_sibling_group_headers(
    executable: &std::ffi::OsStr,
    inventory: &[AgentPane],
    published: &[AgentPane],
    group_heads: &BTreeMap<String, String>,
    workspace_labels: &BTreeMap<String, String>,
    sequence: u64,
    failed: &mut Vec<String>,
) -> Result<usize> {
    let published_ids = published
        .iter()
        .map(|pane| pane.pane_id.as_str())
        .collect::<BTreeSet<_>>();
    let touched = published
        .iter()
        .map(|pane| pane.workspace_id.as_str())
        .filter(|workspace| !workspace.is_empty())
        .collect::<BTreeSet<_>>();
    let mut reported = 0usize;
    for sibling in inventory {
        if published_ids.contains(sibling.pane_id.as_str()) {
            continue;
        }
        if !touched.contains(sibling.workspace_id.as_str()) {
            continue;
        }
        let want = group_label_for(sibling, group_heads, workspace_labels);
        let have = sibling
            .tokens
            .get("quota_group")
            .filter(|value| !value.is_empty())
            .cloned();
        if want == have {
            continue;
        }
        if pane_is_scrolled(executable, &sibling.pane_id) {
            continue;
        }
        reported += 1;
        let mut command = Command::new(executable);
        command
            .args([
                "pane",
                "report-metadata",
                &sibling.pane_id,
                "--source",
                PLUGIN_ID,
            ])
            .args(["--seq", &sequence.to_string()])
            .args(["--ttl-ms", METADATA_TTL_MS]);
        if let Some(label) = &want {
            command.args(["--token", &format!("quota_group={label}")]);
        } else {
            command.args(["--clear-token", "quota_group"]);
        }
        let output = command.output().context("report group header to Herdr")?;
        if !output.status.success() {
            failed.push(sibling.pane_id.clone());
        }
    }
    Ok(reported)
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

/// `w1:p9` → `w1`. Used when an older inventory omits `workspace_id`.
fn workspace_id_from_pane_id(pane_id: &str) -> Option<String> {
    let (workspace, _) = pane_id.split_once(':')?;
    (!workspace.is_empty()).then(|| workspace.to_string())
}

/// Workspace id → label from one `herdr workspace list` call.
fn list_workspace_labels() -> Result<BTreeMap<String, String>> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(&executable)
        .args(["workspace", "list"])
        .output()
        .context("list Herdr workspaces")?;
    if !output.status.success() {
        anyhow::bail!("Herdr workspace list failed with {}", output.status);
    }
    let value: Value = serde_json::from_slice(&output.stdout).context("parse workspace list")?;
    let mut labels = BTreeMap::new();
    collect_workspace_labels(&value, &mut labels);
    Ok(labels)
}

fn collect_workspace_labels(value: &Value, labels: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            let id = map
                .get("workspace_id")
                .or_else(|| map.get("workspaceId"))
                .and_then(Value::as_str);
            let label = map.get("label").and_then(Value::as_str);
            if let (Some(id), Some(label)) = (id, label) {
                labels.insert(id.to_string(), label.to_string());
            }
            for child in map.values() {
                collect_workspace_labels(child, labels);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_workspace_labels(child, labels);
            }
        }
        _ => {}
    }
}

/// Which pane carries the group header for each workspace.
///
/// The header must sit on whichever pane Herdr draws first in the Space.
/// Under Herdr's default grouped order, `inventory` already follows layout
/// order, so the first eligible pane wins. Under this plugin's quota view,
/// Herdr sorts by `quota_stack` and then `quota_headroom`; exact ties keep
/// inventory order because the sort is stable. `quota_stack` is the overlay-
/// aware map produced by `vendor_nesting`, so same-vendor heads stay ahead
/// of their children just as they do in the Agent panel. `publishing` /
/// `tokens` overlay headroom for panes this pass is about to write so a
/// forced refresh can move the header atomically.
fn group_head_pane_ids(
    inventory: &[AgentPane],
    publishing: &[AgentPane],
    tokens: &[PaneTokens],
    quota_order: bool,
    quota_stack: &BTreeMap<String, String>,
    vendor_heads: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let publishing_ids = publishing
        .iter()
        .map(|pane| pane.pane_id.as_str())
        .collect::<BTreeSet<_>>();
    let has_non_vendor_head = |workspace: &str| {
        inventory
            .iter()
            .any(|pane| pane.workspace_id == workspace && !vendor_heads.contains(&pane.pane_id))
            || publishing
                .iter()
                .any(|pane| pane.workspace_id == workspace && !vendor_heads.contains(&pane.pane_id))
    };
    let mut heads: BTreeMap<String, (String, u8, usize, String)> = BTreeMap::new();
    for (index, pane) in inventory.iter().enumerate() {
        if pane.workspace_id.is_empty() {
            continue;
        }
        if vendor_heads.contains(&pane.pane_id) && has_non_vendor_head(&pane.workspace_id) {
            continue;
        }
        let headroom = if !quota_order {
            0
        } else if publishing_ids.contains(pane.pane_id.as_str()) {
            published_headroom(pane, tokens)
        } else {
            pane.tokens
                .get(HEADROOM_TOKEN)
                .and_then(|value| value.parse().ok())
                .unwrap_or(u8::MAX)
        };
        let stack = if quota_order {
            quota_stack
                .get(&pane.pane_id)
                .cloned()
                .or_else(|| pane.tokens.get(STACK_TOKEN).cloned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let candidate = (stack, headroom, index, pane.pane_id.clone());
        match heads.get(&pane.workspace_id) {
            Some(current) if current <= &candidate => {}
            _ => {
                heads.insert(pane.workspace_id.clone(), candidate);
            }
        }
    }
    // A pane present only in this pass (inventory read failed) still needs a
    // head entry so its Space is not left without a label.
    for (index, pane) in publishing.iter().enumerate() {
        if pane.workspace_id.is_empty() || heads.contains_key(&pane.workspace_id) {
            continue;
        }
        let headroom = if quota_order {
            published_headroom(pane, tokens)
        } else {
            0
        };
        let stack = if quota_order {
            quota_stack
                .get(&pane.pane_id)
                .cloned()
                .or_else(|| pane.tokens.get(STACK_TOKEN).cloned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        heads.insert(
            pane.workspace_id.clone(),
            (
                stack,
                headroom,
                inventory.len() + index,
                pane.pane_id.clone(),
            ),
        );
    }
    heads
        .into_iter()
        .map(|(workspace, (_, _, _, pane_id))| (workspace, pane_id))
        .collect()
}

/// Whether the Agent panel is under this plugin's quota-ranked view.
///
/// With `agent-order default`, Herdr owns the ordering and group headers must
/// follow the inventory/layout order. With `quota`, header election must use
/// the same `quota_stack` then `quota_headroom` keys as the Agent view.
fn group_head_uses_quota_order() -> bool {
    let cache = crate::cache::CacheStore::from_env().ok();
    crate::configure::resolved_agent_order(None, cache.as_ref()).is_quota()
}

fn published_headroom(pane: &AgentPane, tokens: &[PaneTokens]) -> u8 {
    tokens
        .iter()
        .find(|tokens| tokens.pane_id == pane.pane_id)
        .and_then(|tokens| match &tokens.quota {
            PaneQuotaUpdate::Replace(values) => values.quota_headroom,
            _ => None,
        })
        .or_else(|| {
            pane.tokens
                .get(HEADROOM_TOKEN)
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(u8::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VendorRow {
    Flat,
    Head,
    Child,
}

struct VendorNesting {
    heads: BTreeSet<String>,
    children: BTreeSet<String>,
    last_children: BTreeSet<String>,
    stack: BTreeMap<String, String>,
    header_icon: BTreeMap<String, AgentStatus>,
}

fn vendor_row_for(wide: bool, nesting: &VendorNesting, pane_id: &str) -> VendorRow {
    if !wide {
        VendorRow::Flat
    } else if nesting.children.contains(pane_id) {
        VendorRow::Child
    } else if nesting.heads.contains(pane_id) {
        VendorRow::Head
    } else {
        VendorRow::Flat
    }
}

fn vendor_icon_status(nesting: &VendorNesting, pane: &AgentPane, role: VendorRow) -> AgentStatus {
    if role == VendorRow::Head {
        nesting
            .header_icon
            .get(&pane.pane_id)
            .copied()
            .unwrap_or(AgentStatus::Idle)
    } else {
        pane.icon_status()
    }
}

fn vendor_nesting(
    inventory: &[AgentPane],
    overlay: &[AgentPane],
    tokens: &[PaneTokens],
) -> VendorNesting {
    let overlay_ids = overlay
        .iter()
        .map(|pane| pane.pane_id.as_str())
        .collect::<BTreeSet<_>>();
    let panes: Vec<&AgentPane> = overlay
        .iter()
        .chain(
            inventory
                .iter()
                .filter(|pane| !overlay_ids.contains(pane.pane_id.as_str())),
        )
        .collect();
    let mut groups: BTreeMap<(String, u8), Vec<&AgentPane>> = BTreeMap::new();
    for pane in &panes {
        if pane.workspace_id.is_empty() || !shares_login_quota(pane.harness) {
            continue;
        }
        groups.entry(nest_group_key(pane)).or_default().push(*pane);
    }
    let mut heads = BTreeSet::new();
    let mut children = BTreeSet::new();
    let mut last_children = BTreeSet::new();
    let mut stack = BTreeMap::new();
    let mut header_icon = BTreeMap::new();
    for ((_, index), members) in &groups {
        let min_head = members
            .iter()
            .map(|pane| published_headroom(pane, tokens))
            .min()
            .unwrap_or(u8::MAX);
        let nested = members.len() >= 2;
        let head_id = vendor_group_head(members);
        if nested {
            heads.insert(head_id.clone());
            header_icon.insert(
                head_id.clone(),
                if members.iter().any(|pane| pane.working()) {
                    AgentStatus::Working
                } else {
                    AgentStatus::Idle
                },
            );
        }
        for pane in members {
            let own = published_headroom(pane, tokens);
            let role = if nested && pane.pane_id != head_id {
                children.insert(pane.pane_id.clone());
                '1'
            } else {
                '0'
            };
            stack.insert(
                pane.pane_id.clone(),
                format!("{min_head:03}{index:02}{role}{own:03}"),
            );
        }
        if nested {
            if let Some(last) = members
                .iter()
                .filter(|pane| pane.pane_id != head_id)
                .max_by_key(|pane| {
                    (
                        stack.get(&pane.pane_id).cloned().unwrap_or_default(),
                        pane.pane_id.as_str(),
                    )
                })
            {
                last_children.insert(last.pane_id.clone());
            }
        }
    }
    for pane in panes {
        stack.entry(pane.pane_id.clone()).or_insert_with(|| {
            let own = published_headroom(pane, tokens);
            format!("{own:03}990{own:03}")
        });
    }
    VendorNesting {
        heads,
        children,
        last_children,
        stack,
        header_icon,
    }
}

pub(crate) fn shares_login_quota(harness: Harness) -> bool {
    matches!(
        harness,
        Harness::Grok | Harness::Codex | Harness::Devin | Harness::OpenCode | Harness::Cursor
    )
}

/// Same-Space same-vendor group. Grok is one login-scoped vendor: the
/// collector reads one `auth.json`, so two Grok tabs in a Space share 5h/7d.
pub(crate) fn nest_group_key(pane: &AgentPane) -> (String, u8) {
    (pane.workspace_id.clone(), harness_stack_index(pane.harness))
}

fn vendor_group_head(members: &[&AgentPane]) -> String {
    members
        .iter()
        .map(|pane| pane.pane_id.as_str())
        .min()
        .unwrap_or_default()
        .to_string()
}

fn harness_stack_index(harness: Harness) -> u8 {
    crate::cli::AgentSelection::SUPPORTED
        .iter()
        .position(|item| *item == harness)
        .unwrap_or(99) as u8
}

fn group_label_for(
    pane: &AgentPane,
    group_heads: &BTreeMap<String, String>,
    workspace_labels: &BTreeMap<String, String>,
) -> Option<String> {
    if pane.workspace_id.is_empty() {
        return None;
    }
    if group_heads
        .get(&pane.workspace_id)
        .is_none_or(|head| head != &pane.pane_id)
    {
        return None;
    }
    workspace_labels
        .get(&pane.workspace_id)
        .cloned()
        .filter(|label| !label.is_empty())
        .or_else(|| {
            let id = pane.workspace_id.clone();
            (!id.is_empty()).then_some(id)
        })
}

/// Herdr hang-indents a head pane's rows under `$quota_group`. Member panes
/// collapse an empty group, so their identity is row 1 and needs the same
/// offset baked into the first plugin cell — same as herdr-radar
/// `group_indent = 2`.
///
/// The indent must ride on the logo token, not a stand-alone `$quota_pad`:
/// Herdr inserts ` · ` between adjacent non-empty tokens, so a pad cell drew
/// a leading middle-dot before the logo. ZWSP + spaces survive trim only when
/// a non-whitespace glyph follows in the same value.
const GROUP_MEMBER_INDENT: &str = "\u{200b}  ";
/// Always reported together so a lagging inventory cannot leave a stale
/// colour twin on screen after working→done or done→idle.
const ICON_TOKEN_NAMES: [&str; 3] = ["quota_icon", "quota_icon_working", "quota_icon_done"];

/// Flat Gauges normally render the packed `$quota_provider_model` identity,
/// so the standalone model token is redundant and can stay out of the report
/// budget. When the provider field is hidden, configure deliberately degrades
/// that identity row to `$quota_model`; keep it only for that exact shape.
fn strip_flat_gauge_model_if_unused(
    desired: &mut BTreeMap<String, String>,
    row: RowStyle,
    role: VendorRow,
) {
    if row.shape.layout != crate::cli::SidebarLayout::Gauges || role != VendorRow::Flat {
        return;
    }
    let model_only_identity = row.fields.contains(crate::cli::SidebarField::Model)
        && !row.fields.contains(crate::cli::SidebarField::Provider);
    if !model_only_identity {
        desired.remove("quota_model");
    }
}

/// Vendor mark always; group header only on the Space head pane.
///
/// `$quota_icon` always carries the glyph so it stays the first identity
/// token. Working/done colour is an invisible suffix matched by Herdr
/// `rules`; publishing a later twin on a Space head hang-indents the mark
/// one cell to the right. Members prefix the logo with
/// [`GROUP_MEMBER_INDENT`]. Heads publish the bare glyph — Herdr already
/// hang-indents their continuation rows. Stale `$quota_pad` from older
/// builds is cleared, as are leftover `_working` / `_done` twins.
fn apply_group_and_icon(
    desired: &mut BTreeMap<String, String>,
    pane: &AgentPane,
    group_heads: &BTreeMap<String, String>,
    workspace_labels: &BTreeMap<String, String>,
    role: VendorRow,
    icon_status: Option<AgentStatus>,
) {
    let glyph = crate::icons::for_harness(pane.harness);
    let space_head = group_heads
        .get(&pane.workspace_id)
        .is_some_and(|head| head == &pane.pane_id);
    let member = !pane.workspace_id.is_empty() && !space_head;
    if role == VendorRow::Child {
        for token in ICON_TOKEN_NAMES {
            desired.remove(token);
        }
        desired.remove("quota_provider");
        desired.remove("quota_provider_model");
        desired.remove(NEST_GAP_TOKEN);
        if member {
            indent_token(desired, "quota_model");
        }
    } else {
        let mut mark = if member {
            format!("{GROUP_MEMBER_INDENT}{glyph}")
        } else {
            glyph.to_string()
        };
        match icon_status.unwrap_or_else(|| pane.icon_status()) {
            AgentStatus::Working => mark.push_str(crate::icons::WORKING_TAG),
            AgentStatus::Done => mark.push_str(crate::icons::DONE_TAG),
            _ => {}
        }
        desired.insert("quota_icon".to_string(), mark);
        desired.remove("quota_icon_working");
        desired.remove("quota_icon_done");
        desired.remove(NEST_GAP_TOKEN);
    }
    desired.remove("quota_pad");
    // Never preserve a previous header: non-heads must omit the token so the
    // report clears it. Blind preserve is what left `ifs` on two panes.
    if let Some(label) = group_label_for(pane, group_heads, workspace_labels) {
        desired.insert("quota_group".to_string(), label);
    } else {
        desired.remove("quota_group");
    }
}

fn desired_tokens(
    values: &MetadataTokens,
    topic: &str,
    shape: SidebarShape,
) -> BTreeMap<String, String> {
    let mut tokens = BTreeMap::new();
    insert_optional_token(&mut tokens, "quota_provider", &values.quota_provider);
    tokens.insert(
        "quota_provider_model".to_string(),
        values.quota_provider_model.clone(),
    );
    insert_optional_token(&mut tokens, "quota_model", &values.quota_model);
    insert_context_token(
        &mut tokens,
        &values.quota_context,
        values.quota_context_severity,
        shape,
    );
    insert_optional_token(&mut tokens, "quota_cache", &values.quota_cache);
    insert_optional_token(&mut tokens, "quota_cache_ttl", &values.quota_cache_ttl);
    insert_optional_token(&mut tokens, "quota_cache_state", &values.quota_cache_state);
    let week_base = week_style_base(&values.quota_5h, &values.quota_context);
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
    insert_severity_token(
        &mut tokens,
        "quota_month",
        &values.quota_month,
        values.quota_month_severity,
    );
    insert_optional_token(&mut tokens, "quota_topic", topic);
    if let Some(error) = &values.quota_error {
        tokens.insert("quota_error".to_string(), error.clone());
    }
    if let Some(headroom) = values.quota_headroom {
        tokens.insert(HEADROOM_TOKEN.to_string(), format!("{headroom:03}"));
    }
    tokens
}

fn display_topic(pane: &AgentPane) -> String {
    let topic = pane.topic.trim();
    if topic.is_empty() || is_status_line(topic) || is_help_command_row(topic) {
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

fn identity_is_narrow(content_width: usize) -> bool {
    content_width > 0 && content_width < 22
}

fn publish_content_width() -> usize {
    crate::presentation::SidebarShape::new(
        crate::cli::SidebarLayout::default(),
        crate::configure::herdr::sidebar_width(),
    )
    .content_width
}

fn apply_identity(
    tokens: &mut BTreeMap<String, String>,
    identity: &PaneIdentity,
    content_width: usize,
    role: VendorRow,
) {
    let narrow = identity_is_narrow(content_width);
    if !narrow && role == VendorRow::Head {
        apply_nested_head_identity(tokens, identity);
        return;
    }
    if !narrow && role == VendorRow::Child {
        apply_nested_child_identity(tokens, identity);
        return;
    }
    if identity.model.is_empty() {
        tokens.remove("quota_model");
        tokens.insert("quota_provider".to_string(), identity.provider.clone());
        tokens.insert(
            "quota_provider_model".to_string(),
            identity.provider.clone(),
        );
        return;
    }
    tokens.insert("quota_model".to_string(), identity.model.clone());
    if narrow {
        // Logo already says who; keep the model only and collapse a stacked
        // provider row so it does not re-introduce the prefix.
        tokens.remove("quota_provider");
        tokens.insert("quota_provider_model".to_string(), identity.model.clone());
    } else {
        tokens.insert("quota_provider".to_string(), identity.provider.clone());
        tokens.insert(
            "quota_provider_model".to_string(),
            format!("{}/{}", identity.provider, identity.model),
        );
    }
}

fn apply_nested_head_identity(tokens: &mut BTreeMap<String, String>, identity: &PaneIdentity) {
    if identity.model.is_empty() {
        tokens.remove("quota_model");
    } else {
        tokens.insert("quota_model".to_string(), identity.model.clone());
    }
    tokens.insert("quota_provider".to_string(), identity.provider.clone());
    tokens.insert(
        "quota_provider_model".to_string(),
        identity.provider.clone(),
    );
}

fn apply_nested_head_from_tokens(tokens: &mut BTreeMap<String, String>) {
    let provider = tokens
        .get("quota_provider")
        .cloned()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            tokens.get("quota_provider_model").and_then(|label| {
                label
                    .split_once('/')
                    .map(|(provider, _)| provider.to_string())
            })
        })
        .unwrap_or_default();
    let model = tokens
        .get("quota_model")
        .cloned()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            tokens.get("quota_provider_model").and_then(|label| {
                label
                    .split_once('/')
                    .map(|(_, model)| model.to_string())
                    .filter(|model| !model.is_empty())
            })
        });
    if let Some(model) = model {
        tokens.insert("quota_model".to_string(), model);
    }
    if provider.is_empty() {
        return;
    }
    tokens.insert("quota_provider".to_string(), provider.clone());
    tokens.insert("quota_provider_model".to_string(), provider);
}

fn indent_token(tokens: &mut BTreeMap<String, String>, name: &str) {
    let Some(value) = tokens.get(name) else {
        return;
    };
    if value.is_empty() || value.starts_with('\u{200b}') {
        return;
    }
    tokens.insert(name.to_string(), format!("{GROUP_MEMBER_INDENT}{value}"));
}

fn pack_gap_enabled() -> bool {
    crate::cache::CacheStore::from_env()
        .ok()
        .and_then(|cache| cache.row_gap())
        .unwrap_or_default()
        == crate::cli::SidebarRowGap::SEPARATED
}

fn apply_pack_gap(
    desired: &mut BTreeMap<String, String>,
    role: VendorRow,
    nesting: &VendorNesting,
    pane_id: &str,
    separated: bool,
) {
    if separated && pane_takes_pack_gap(role, nesting, pane_id) {
        desired.insert(NEST_GAP_TOKEN.to_string(), NEST_GAP_VALUE.to_string());
    } else {
        desired.remove(NEST_GAP_TOKEN);
    }
}

fn pane_takes_pack_gap(role: VendorRow, nesting: &VendorNesting, pane_id: &str) -> bool {
    match role {
        VendorRow::Head => false,
        VendorRow::Child => nesting.last_children.contains(pane_id),
        VendorRow::Flat => true,
    }
}

fn strip_vendor_head_session(tokens: &mut BTreeMap<String, String>) {
    tokens.remove("quota_cache");
    tokens.remove("quota_cache_ttl");
    tokens.remove("quota_cache_state");
    for name in ACCOUNT_QUOTA_TOKEN_NAMES {
        unindent_token(tokens, name);
    }
    unindent_token(tokens, "quota_model");
}

fn strip_vendor_child_extras(tokens: &mut BTreeMap<String, String>) {
    tokens.remove("quota_cache");
    tokens.remove("quota_cache_ttl");
    tokens.remove("quota_cache_state");
    unindent_token(tokens, "quota_topic");
    for name in CONTEXT_TOKEN_NAMES {
        unindent_token(tokens, name);
    }
}

fn unindent_token(tokens: &mut BTreeMap<String, String>, name: &str) {
    let Some(value) = tokens.get(name) else {
        return;
    };
    if !value.starts_with('\u{200b}') {
        return;
    }
    let cleaned = value.trim_start_matches(['\u{200b}', ' ']).to_string();
    if cleaned.is_empty() {
        tokens.remove(name);
    } else {
        tokens.insert(name.to_string(), cleaned);
    }
}

fn apply_nested_child_identity(tokens: &mut BTreeMap<String, String>, identity: &PaneIdentity) {
    tokens.remove("quota_provider");
    tokens.remove("quota_provider_model");
    if identity.model.is_empty() {
        tokens.remove("quota_model");
        return;
    }
    tokens.insert("quota_model".to_string(), identity.model.clone());
}

fn apply_nested_child_from_tokens(tokens: &mut BTreeMap<String, String>) {
    let model = tokens
        .get("quota_model")
        .cloned()
        .filter(|model| !model.is_empty())
        .or_else(|| {
            let label = tokens.get("quota_provider_model")?;
            if let Some((_, model)) = label.split_once('/') {
                let model = model.trim();
                (!model.is_empty()).then(|| model.to_string())
            } else {
                let label = label.trim();
                (!label.is_empty()).then(|| label.to_string())
            }
        });
    tokens.remove("quota_provider");
    tokens.remove("quota_provider_model");
    if let Some(model) = model {
        tokens.insert("quota_model".to_string(), model);
    } else {
        tokens.remove("quota_model");
    }
}

fn apply_context(
    tokens: &mut BTreeMap<String, String>,
    context: &ContextUsage,
    now_unix: u64,
    row: RowStyle,
) {
    insert_context_token(
        tokens,
        &crate::presentation::sidebar_context(Some(context), row.percent, row.shape),
        Some(crate::presentation::context_severity(context, row.percent)),
        row.shape,
    );
    let cache = crate::presentation::sidebar_cache(Some(context));
    if cache.is_empty() {
        tokens.remove("quota_cache");
    } else {
        tokens.insert("quota_cache".to_string(), cache);
    }
    for (name, value) in [
        (
            "quota_cache_ttl",
            crate::presentation::sidebar_cache_ttl(Some(context), now_unix),
        ),
        (
            "quota_cache_state",
            crate::presentation::sidebar_cache_state(Some(context), now_unix),
        ),
    ] {
        if value.is_empty() {
            tokens.remove(name);
        } else {
            tokens.insert(name.to_string(), value);
        }
    }
}

/// Keep the configured rows fixed. An empty TTL token collapses its row when
/// both visible fields fit inside the cache token; the next refresh can split
/// them again from the session evidence without rewriting Herdr's config.
///
/// `no cached` is not folded here: it keeps `$quota_cache_state` so the amber
/// warning colour survives. Gauges puts that token on the cache row.
pub(crate) fn strip_account_quota_tokens(tokens: &mut BTreeMap<String, String>) {
    for name in ACCOUNT_QUOTA_TOKEN_NAMES {
        tokens.remove(name);
    }
    tokens.retain(|name, _| !name.starts_with("quota_share_"));
}

fn share_token_name(name: &str) -> String {
    format!(
        "quota_share_{}",
        name.strip_prefix("quota_").unwrap_or(name)
    )
}

fn current_quota_window<'a>(
    tokens: &'a BTreeMap<String, String>,
    name: &'static str,
) -> Option<&'a String> {
    tokens
        .get(name)
        .or_else(|| tokens.get(&share_token_name(name)))
}

fn promote_shared_quota(tokens: &mut BTreeMap<String, String>) {
    for name in ACCOUNT_QUOTA_TOKEN_NAMES {
        if let Some(value) = tokens.remove(name) {
            tokens.insert(share_token_name(name), value);
        }
    }
}

fn fold_cache_row(tokens: &mut BTreeMap<String, String>, row: RowStyle) {
    use crate::cli::{SidebarField, SidebarLayout};
    if row.shape.layout != SidebarLayout::Gauges
        || !row.fields.contains(SidebarField::Cache)
        || !row.fields.contains(SidebarField::Ttl)
    {
        return;
    }
    let (Some(cache), Some(ttl)) = (tokens.get("quota_cache"), tokens.get("quota_cache_ttl"))
    else {
        return;
    };
    let joined = format!("{cache} · {ttl}");
    // These are plugin-generated numeric labels; · and ≈ each occupy one cell.
    if joined.chars().count() <= row.shape.content_width {
        tokens.insert("quota_cache".to_string(), joined);
        tokens.remove("quota_cache_ttl");
    }
}

/// True when the quota rows this pane is carrying are not the ones `values`
/// would render.
///
/// Only the window rows are comparable against a snapshot alone: publishing
/// rewrites provider, model, context and cache rows from pane-local evidence
/// this caller does not have, so including them would report drift that no
/// republish can settle.
///
/// A pane that has never been published to is not drift: waking those would
/// pull every quota-less pane into every pass. A pane that already carries
/// plugin tokens but no window row is drift once the snapshot has windows —
/// Claude/Agy statusLine only writes the mailbox, so an idle pane has no
/// other way to pick up a session that just reported quota.
pub(crate) fn quota_rows_have_drifted(
    current: &BTreeMap<String, String>,
    values: &MetadataTokens,
    shape: SidebarShape,
) -> bool {
    let desired = desired_tokens(values, "", shape);
    let current_has_window = QUOTA_WINDOW_TOKEN_NAMES
        .into_iter()
        .any(|name| current.contains_key(name))
        || current.keys().any(|name| name.starts_with("quota_share_"));
    let desired_has_window = QUOTA_WINDOW_TOKEN_NAMES
        .into_iter()
        .any(|name| desired.contains_key(name));
    if !current_has_window {
        return desired_has_window && plugin_quota_present(current);
    }
    QUOTA_WINDOW_TOKEN_NAMES
        .into_iter()
        .any(|name| current_quota_window(current, name) != desired.get(name))
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
    // Icon twins must always be named so an inactive colour is cleared even
    // when `agent list` omitted the stale token from `pane.tokens`.
    for name in ICON_TOKEN_NAMES {
        if !names.contains(&name) {
            names.push(name);
        }
    }
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
    // names first so an upgraded pane can actually clear them; an unchanged
    // value is re-sent on the next bounded report instead.
    let active_capacity = MAX_METADATA_TOKENS.saturating_sub(cleanup_names.len());
    while names.len() > active_capacity {
        let Some(index) = names.iter().position(|name| {
            // Dropping a name the pane still carries but no longer wants would
            // leave that row on screen forever, so those are never given up.
            // Icon twins are never dropped either — a stale colour is worse
            // than a briefly lagged quota digit.
            let must_clear = pane.tokens.contains_key(*name) && !desired.contains_key(*name);
            let is_icon = ICON_TOKEN_NAMES.contains(name);
            // Nested-head 5h/7d/30d live on quota_share_*; dropping them to
            // squeeze the 16-token budget hides the only visible windows.
            !must_clear
                && !is_icon
                && !ROWS_THAT_MUST_NOT_LAG.contains(name)
                && !name.starts_with("quota_share_")
        }) else {
            break;
        };
        names.remove(index);
    }
    names.truncate(active_capacity);
    names.extend(cleanup_names);
    names
}

fn week_style_base(quota_5h: &str, quota_context: &str) -> &'static str {
    // Empty 5h publishes week beside context (`context · 7d`) when that row
    // exists. Without context the inline token has nothing to hang on, so week
    // stays on the limits row.
    if quota_5h.trim().is_empty() && !quota_context.trim().is_empty() {
        "quota_week_inline"
    } else {
        "quota_week"
    }
}

/// Publish the context row into the one name its layout and severity choose,
/// and clear the other three.
///
/// The clear is the point: a severity change or a layout switch moves the
/// value to a different name, and a pane that kept the old one would show two
/// context rows at once.
fn insert_context_token(
    tokens: &mut BTreeMap<String, String>,
    value: &str,
    severity: Option<crate::model::Severity>,
    shape: SidebarShape,
) {
    for name in CONTEXT_TOKEN_NAMES {
        tokens.remove(name);
    }
    if value.trim().is_empty() {
        return;
    }
    tokens.insert(
        context_token_name(shape, severity).to_string(),
        value.to_string(),
    );
}

fn context_token_name(
    shape: SidebarShape,
    severity: Option<crate::model::Severity>,
) -> &'static str {
    if shape.layout != crate::cli::SidebarLayout::Gauges {
        return "quota_context";
    }
    // `Severity::for_context_remaining` never returns `Unknown`, and a caller
    // with no severity has no coloured band to claim, so both read as normal.
    match severity {
        Some(crate::model::Severity::Warning) => "quota_context_warning",
        Some(crate::model::Severity::Danger) => "quota_context_danger",
        _ => "quota_context_normal",
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
        crate::model::Severity::Normal => "normal",
        crate::model::Severity::Warning => "warning",
        crate::model::Severity::Danger => "danger",
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
        if candidate.is_empty() || is_status_line(candidate) || is_help_command_row(candidate) {
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
            "/clear"
                | "/compact"
                | "/help"
                | "/status"
                | "/usage"
                | "/model"
                | "/config"
                | "/login"
        )
}

/// Grok Build help rows look like `/login     Log in or re-authenticate…`.
/// A real prompt after a slash command has a single space then the text
/// (`/goal 你在 ti…`).
fn is_help_command_row(value: &str) -> bool {
    let trimmed = value.trim();
    if !trimmed.starts_with('/') {
        return false;
    }
    let rest = trimmed.trim_start_matches(|character: char| !character.is_whitespace());
    rest.is_empty() || rest.starts_with("  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{FieldSet, PercentStyle, SidebarField, SidebarLayout};
    use crate::model::{
        CacheUsage, ContextUsage, ProviderSnapshot, ResetAt, UsageWindow, WindowKind,
    };
    use serde_json::json;

    #[test]
    fn codex_resume_session_id_accepts_exact_uuid_with_supported_flags() {
        let id = "019f6908-3bc1-7c83-98df-d8ea91694d2c";
        for argv in [
            vec!["codex", "resume", id],
            vec![
                "/opt/homebrew/bin/codex",
                "--model",
                "gpt-5.6-codex",
                "resume",
                "--all",
                id,
            ],
            vec!["codex", "resume", "--include-non-interactive", id],
            vec!["codex", "resume", "--model", "gpt-5.6-codex", id],
            vec!["codex", "--model=gpt-5.6-codex", "resume", id],
        ] {
            assert_eq!(
                codex_resume_session_id(&json!({"argv": argv})).as_deref(),
                Some(id)
            );
        }
    }

    #[test]
    fn codex_resume_session_id_fails_closed_without_an_exact_uuid_target() {
        let id = "019f6908-3bc1-7c83-98df-d8ea91694d2c";
        for argv in [
            vec!["codex", "resume", "--last"],
            vec!["codex", "resume", "named-session"],
            vec!["codex", "resume", "not-a-uuid"],
            vec!["codex", "resume", "--future-option", id],
        ] {
            assert_eq!(codex_resume_session_id(&json!({"argv": argv})), None);
        }
    }

    #[test]
    fn muse_sessions_fill_only_session_less_muse_panes() {
        let pane = |id: &str, harness: Harness, session: Option<&str>| AgentPane {
            pane_id: id.to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness,
            session: session.map(|value| AgentSession {
                kind: Some("id".to_string()),
                value: value.to_string(),
            }),
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
        };
        let mut panes = vec![
            pane("w1:p1", Harness::Muse, None),
            pane("w1:p2", Harness::Muse, Some("herdr-session")),
            pane("w1:p3", Harness::Claude, None),
        ];
        let mut asked = Vec::new();
        attach_muse_sessions_with(&mut panes, |pane_ids| {
            asked = pane_ids.to_vec();
            ["w1:p1", "w1:p2", "w1:p3"]
                .into_iter()
                .map(|id| (id.to_string(), format!("lock-{id}")))
                .collect()
        });
        assert_eq!(asked, vec!["w1:p1".to_string()]);
        assert_eq!(
            panes[0].session.as_ref().and_then(AgentSession::id),
            Some("lock-w1:p1")
        );
        assert_eq!(
            panes[1].session.as_ref().and_then(AgentSession::id),
            Some("herdr-session")
        );
        assert_eq!(panes[2].session, None);

        let mut without_muse = vec![pane("w1:p3", Harness::Claude, None)];
        attach_muse_sessions_with(&mut without_muse, |_| {
            panic!("a pane list without a session-less Muse pane never resolves")
        });
    }

    /// Herdr orders an Agent view by the token's own value, so the padding is
    /// the whole contract: `007` must sort before `042`, and `100` last.
    #[test]
    fn the_headroom_token_is_padded_so_its_text_order_is_its_numeric_order() {
        let token = |headroom: Option<u8>| {
            let mut values = MetadataTokens::unavailable(Provider::Claude, "test");
            values.quota_headroom = headroom;
            desired_tokens(&values, "", SidebarShape::default())
                .get(HEADROOM_TOKEN)
                .cloned()
        };
        assert_eq!(token(Some(7)).as_deref(), Some("007"));
        assert_eq!(token(Some(42)).as_deref(), Some("042"));
        assert_eq!(token(Some(100)).as_deref(), Some("100"));
        assert_eq!(token(None), None);

        let mut sorted = ["100", "007", "042", "000"];
        sorted.sort_unstable();
        assert_eq!(sorted, ["000", "007", "042", "100"]);
    }

    /// The comparison set and the report set are the same list, so a token
    /// that is published but not listed silently stops being compared and
    /// every refresh becomes a write.
    #[test]
    fn the_headroom_token_is_listed_among_the_names_that_are_compared() {
        assert!(METADATA_TOKEN_NAMES.contains(&HEADROOM_TOKEN));
        assert!(!OBSOLETE_METADATA_TOKEN_NAMES.contains(&HEADROOM_TOKEN));
        assert!(METADATA_TOKEN_NAMES.contains(&STACK_TOKEN));
        assert!(METADATA_TOKEN_NAMES.contains(&NEST_GAP_TOKEN));
        assert!(ROWS_THAT_MUST_NOT_LAG.contains(&STACK_TOKEN));
        assert!(ROWS_THAT_MUST_NOT_LAG.contains(&NEST_GAP_TOKEN));
        assert!(
            !NEST_GAP_VALUE.trim().is_empty(),
            "Herdr trims whitespace-only tokens; NBSP would collapse the blank row"
        );
    }

    #[test]
    fn a_nested_head_does_not_insert_a_blank_after_quota() {
        let pane = AgentPane {
            pane_id: "w5:pA".to_string(),
            workspace_id: "w5".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
        };
        let mut desired = BTreeMap::new();
        apply_group_and_icon(
            &mut desired,
            &pane,
            &BTreeMap::new(),
            &BTreeMap::new(),
            VendorRow::Head,
            None,
        );
        assert!(!desired.contains_key(NEST_GAP_TOKEN));
    }

    #[test]
    fn same_space_vendor_children_stack_after_the_head() {
        let focused = AgentPane {
            pane_id: "w5:pA".to_string(),
            workspace_id: "w5".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([(HEADROOM_TOKEN.to_string(), "046".to_string())]),
            status: AgentStatus::Idle,
            focused: true,
        };
        let mut extra = AgentPane {
            pane_id: "w5:pD".to_string(),
            workspace_id: "w5".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([(HEADROOM_TOKEN.to_string(), "046".to_string())]),
            status: AgentStatus::Idle,
            focused: false,
        };
        extra.focused = true;
        extra.status = AgentStatus::Working;
        let inventory = vec![focused.clone(), extra.clone()];
        let nesting = vendor_nesting(&inventory, &inventory, &[]);
        assert!(nesting.children.contains("w5:pD"));
        assert!(nesting.heads.contains("w5:pA"));
        assert!(
            !nesting.heads.contains("w5:pD"),
            "focus must not steal the shared vendor header"
        );
        assert!(!nesting.children.contains("w5:pA"));
        let head = nesting.stack.get("w5:pA").expect("head");
        let child = nesting.stack.get("w5:pD").expect("child");
        assert!(head < child, "{head} should sort before {child}");
        assert_eq!(&head[..5], &child[..5], "same vendor group prefix");
        assert_eq!(
            nesting.header_icon.get("w5:pA").copied(),
            Some(AgentStatus::Working)
        );
        assert_eq!(
            nesting.last_children.iter().collect::<Vec<_>>(),
            vec!["w5:pD"]
        );
        let mut head_tokens = BTreeMap::new();
        apply_pack_gap(&mut head_tokens, VendorRow::Head, &nesting, "w5:pA", true);
        assert!(!head_tokens.contains_key(NEST_GAP_TOKEN));
        let mut child_tokens = BTreeMap::new();
        apply_pack_gap(&mut child_tokens, VendorRow::Child, &nesting, "w5:pD", true);
        assert_eq!(
            child_tokens.get(NEST_GAP_TOKEN).map(String::as_str),
            Some(NEST_GAP_VALUE)
        );
        let mut flushed = BTreeMap::new();
        apply_pack_gap(&mut flushed, VendorRow::Child, &nesting, "w5:pD", false);
        assert!(!flushed.contains_key(NEST_GAP_TOKEN));
    }

    #[test]
    fn claude_panes_in_one_space_do_not_nest() {
        let panes = vec![
            AgentPane {
                pane_id: "w1:p1".to_string(),
                workspace_id: "w1".to_string(),
                cwd: String::new(),
                title: String::new(),
                harness: Harness::Claude,
                session: None,
                session_summary: String::new(),
                topic: String::new(),
                tokens: BTreeMap::new(),
                status: AgentStatus::Idle,
                focused: false,
            },
            AgentPane {
                pane_id: "w1:p2".to_string(),
                workspace_id: "w1".to_string(),
                cwd: String::new(),
                title: String::new(),
                harness: Harness::Claude,
                session: None,
                session_summary: String::new(),
                topic: String::new(),
                tokens: BTreeMap::new(),
                status: AgentStatus::Idle,
                focused: false,
            },
        ];
        let nesting = vendor_nesting(&panes, &panes, &[]);
        assert!(nesting.heads.is_empty(), "{:?}", nesting.heads);
        assert!(nesting.children.is_empty(), "{:?}", nesting.children);
    }

    #[test]
    fn a_shared_vendor_header_stays_idle_when_a_session_completes() {
        let head = AgentPane {
            pane_id: "w5:pA".to_string(),
            workspace_id: "w5".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Done,
            focused: false,
        };
        let extra = AgentPane {
            pane_id: "w5:pD".to_string(),
            workspace_id: "w5".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
        };
        let inventory = vec![head.clone(), extra];
        let nesting = vendor_nesting(&inventory, &inventory, &[]);
        assert_eq!(
            nesting.header_icon.get("w5:pA").copied(),
            Some(AgentStatus::Idle)
        );
    }

    #[test]
    fn codex_vendor_child_keeps_its_identity_and_context_without_account_quota() {
        let pane = AgentPane {
            pane_id: "w5:pD".to_string(),
            workspace_id: "w5".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Codex,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
        };
        let mut desired = BTreeMap::from([
            ("quota_provider".to_string(), "Codex".to_string()),
            (
                "quota_provider_model".to_string(),
                "Codex/gpt-5.6-codex".to_string(),
            ),
            ("quota_model".to_string(), "gpt-5.6-codex".to_string()),
            ("quota_topic".to_string(), "child work".to_string()),
            ("quota_context_normal".to_string(), "cx 42%".to_string()),
            ("quota_5h_normal".to_string(), "5h 80%".to_string()),
            ("quota_week_normal".to_string(), "7d 70%".to_string()),
            ("quota_month_normal".to_string(), "30d 60%".to_string()),
        ]);

        // This mirrors the Codex-child remap in publish_pane_tokens_inner:
        // head semantics for identity, child semantics for account quota, and
        // flat semantics for the icon.
        apply_identity(
            &mut desired,
            &PaneIdentity {
                provider: "Codex".to_string(),
                model: "gpt-5.6-codex".to_string(),
            },
            30,
            VendorRow::Head,
        );
        strip_account_quota_tokens(&mut desired);
        strip_vendor_child_extras(&mut desired);
        let heads = BTreeMap::from([("w5".to_string(), "w5:p1".to_string())]);
        apply_group_and_icon(
            &mut desired,
            &pane,
            &heads,
            &BTreeMap::new(),
            VendorRow::Flat,
            None,
        );

        assert!(desired.contains_key("quota_icon"), "{desired:?}");
        assert_eq!(
            desired.get("quota_provider").map(String::as_str),
            Some("Codex")
        );
        assert_eq!(
            desired.get("quota_provider_model").map(String::as_str),
            Some("Codex")
        );
        assert_eq!(
            desired.get("quota_model").map(String::as_str),
            Some("gpt-5.6-codex")
        );
        assert_eq!(
            desired.get("quota_context_normal").map(String::as_str),
            Some("cx 42%")
        );
        assert_eq!(
            desired.get("quota_topic").map(String::as_str),
            Some("child work")
        );
        for name in ACCOUNT_QUOTA_TOKEN_NAMES {
            assert!(!desired.contains_key(name), "{name}: {desired:?}");
        }
        assert!(
            desired.keys().all(|name| !name.starts_with("quota_share_")),
            "{desired:?}"
        );
    }

    #[test]
    fn flat_gauges_keep_model_when_provider_field_is_hidden() {
        let model = ("quota_model".to_string(), "Opus 5.5".to_string());
        let row = RowStyle {
            percent: PercentStyle::Remaining,
            shape: SidebarShape::new(SidebarLayout::Gauges, 36),
            fields: FieldSet::all().toggled(SidebarField::Provider),
            pacing: Default::default(),
        };
        let mut desired = BTreeMap::from([model.clone()]);
        strip_flat_gauge_model_if_unused(&mut desired, row, VendorRow::Flat);
        assert_eq!(
            desired.get("quota_model").map(String::as_str),
            Some("Opus 5.5")
        );

        let mut default_fields = BTreeMap::from([model]);
        strip_flat_gauge_model_if_unused(
            &mut default_fields,
            RowStyle {
                fields: FieldSet::all(),
                ..row
            },
            VendorRow::Flat,
        );
        assert!(
            !default_fields.contains_key("quota_model"),
            "packed provider/model identity should still omit the redundant token"
        );
    }

    #[test]
    fn wide_vendor_child_omits_provider_and_indents_model() {
        let mut tokens = BTreeMap::new();
        apply_identity(
            &mut tokens,
            &PaneIdentity {
                provider: "Grok".to_string(),
                model: "grok-4.6".to_string(),
            },
            30,
            VendorRow::Child,
        );
        assert!(!tokens.contains_key("quota_provider"));
        assert!(!tokens.contains_key("quota_provider_model"));
        assert_eq!(
            tokens.get("quota_model").map(String::as_str),
            Some("grok-4.6")
        );
        let pane = AgentPane {
            pane_id: "w5:pD".to_string(),
            workspace_id: "w5".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
        };
        let heads = BTreeMap::from([("w5".to_string(), "w5:p1".to_string())]);
        let mut desired = tokens.clone();
        apply_group_and_icon(
            &mut desired,
            &pane,
            &heads,
            &BTreeMap::new(),
            VendorRow::Child,
            None,
        );
        assert!(
            !desired.contains_key("quota_icon")
                && !desired.contains_key("quota_icon_working")
                && !desired.contains_key("quota_icon_done"),
            "child rows have no brand icon: {desired:?}"
        );
        assert_eq!(
            desired.get("quota_model").map(String::as_str),
            Some(concat!("\u{200b}  ", "grok-4.6")),
            "child model uses the Space member indent: {desired:?}"
        );
        assert!(!desired.contains_key("quota_provider_model"));
    }

    #[test]
    fn wide_vendor_head_keeps_provider_and_quota_only() {
        let mut tokens = BTreeMap::from([
            ("quota_provider".to_string(), "Grok".to_string()),
            (
                "quota_provider_model".to_string(),
                "Grok/grok-4.6".to_string(),
            ),
            ("quota_model".to_string(), "grok-4.6".to_string()),
            ("quota_topic".to_string(), "hello".to_string()),
            ("quota_context_normal".to_string(), "cx 10%".to_string()),
            ("quota_week_normal".to_string(), "7d 46%".to_string()),
        ]);
        apply_identity(
            &mut tokens,
            &PaneIdentity {
                provider: "Grok".to_string(),
                model: "grok-4.6".to_string(),
            },
            30,
            VendorRow::Head,
        );
        strip_vendor_head_session(&mut tokens);
        assert_eq!(
            tokens.get("quota_provider_model").map(String::as_str),
            Some("Grok")
        );
        assert_eq!(
            tokens.get("quota_model").map(String::as_str),
            Some("grok-4.6")
        );
        assert_eq!(tokens.get("quota_topic").map(String::as_str), Some("hello"));
        assert_eq!(
            tokens.get("quota_context_normal").map(String::as_str),
            Some("cx 10%")
        );
        assert_eq!(
            tokens.get("quota_week_normal").map(String::as_str),
            Some("7d 46%")
        );
    }

    #[test]
    fn narrow_vendor_child_does_not_add_a_second_indent() {
        let pane = AgentPane {
            pane_id: "w5:pD".to_string(),
            workspace_id: "w5".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
        };
        let heads = BTreeMap::from([("w5".to_string(), "w5:p1".to_string())]);
        let mut desired = BTreeMap::new();
        apply_group_and_icon(
            &mut desired,
            &pane,
            &heads,
            &BTreeMap::new(),
            VendorRow::Flat,
            None,
        );
        assert!(
            desired
                .get("quota_icon")
                .is_some_and(|icon| icon.starts_with(GROUP_MEMBER_INDENT)),
            "{desired:?}"
        );
    }

    /// A one-pane publish still knows the Space head from inventory, and the
    /// non-head drops `$quota_group` instead of preserving a stale label.
    #[test]
    fn group_header_lands_on_the_tightest_pane_and_clears_siblings() {
        let head = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Codex,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([
                (HEADROOM_TOKEN.to_string(), "000".to_string()),
                ("quota_group".to_string(), "ifs".to_string()),
            ]),
            status: AgentStatus::Idle,
            focused: false,
        };
        let sibling = AgentPane {
            pane_id: "w1:p2".to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([
                (HEADROOM_TOKEN.to_string(), "016".to_string()),
                ("quota_group".to_string(), "ifs".to_string()),
            ]),
            status: AgentStatus::Idle,
            focused: false,
        };
        let inventory = vec![head.clone(), sibling.clone()];
        let nesting = vendor_nesting(&inventory, std::slice::from_ref(&sibling), &[]);
        let heads = group_head_pane_ids(
            &inventory,
            std::slice::from_ref(&sibling),
            &[],
            true,
            &nesting.stack,
            &BTreeSet::new(),
        );
        assert_eq!(heads.get("w1").map(String::as_str), Some("w1:p1"));

        // Default order follows Herdr's inventory/layout order, even when a
        // later pane has less quota remaining.
        let reversed = vec![sibling.clone(), head.clone()];
        let reversed_nesting = vendor_nesting(&reversed, &[], &[]);
        let default_heads = group_head_pane_ids(
            &reversed,
            &[],
            &[],
            false,
            &reversed_nesting.stack,
            &BTreeSet::new(),
        );
        assert_eq!(default_heads.get("w1").map(String::as_str), Some("w1:p2"));
        let quota_heads = group_head_pane_ids(
            &reversed,
            &[],
            &[],
            true,
            &reversed_nesting.stack,
            &BTreeSet::new(),
        );
        assert_eq!(quota_heads.get("w1").map(String::as_str), Some("w1:p1"));

        // Exact quota-sort ties keep Herdr's stable inventory order. Use
        // non-nested harnesses so quota_stack is identical for both panes.
        let mut stable_first = sibling.clone();
        stable_first.harness = Harness::Claude;
        let mut stable_late = head.clone();
        stable_late.pane_id = "w1:p10".to_string();
        stable_late.harness = Harness::Agy;
        stable_late
            .tokens
            .insert(HEADROOM_TOKEN.to_string(), "016".to_string());
        let equal_inventory = vec![stable_first, stable_late];
        let equal_nesting = vendor_nesting(&equal_inventory, &[], &[]);
        let equal_heads = group_head_pane_ids(
            &equal_inventory,
            &[],
            &[],
            true,
            &equal_nesting.stack,
            &BTreeSet::new(),
        );
        assert_eq!(equal_heads.get("w1").map(String::as_str), Some("w1:p2"));

        // Same-vendor equal-headroom panes are not an exact sort tie:
        // quota_stack deliberately puts the stable vendor head before its
        // child. That can differ from inventory order (p10 sorts before p7).
        let mut layout_first = sibling.clone();
        layout_first.pane_id = "w1:p7".to_string();
        layout_first.harness = Harness::Cursor;
        layout_first
            .tokens
            .insert(HEADROOM_TOKEN.to_string(), "016".to_string());
        let mut vendor_head = sibling.clone();
        vendor_head.pane_id = "w1:p10".to_string();
        vendor_head.harness = Harness::Cursor;
        vendor_head
            .tokens
            .insert(HEADROOM_TOKEN.to_string(), "016".to_string());
        let vendor_inventory = vec![layout_first, vendor_head];
        let vendor_nesting = vendor_nesting(&vendor_inventory, &[], &[]);
        assert!(vendor_nesting.heads.contains("w1:p10"));
        let vendor_default_heads = group_head_pane_ids(
            &vendor_inventory,
            &[],
            &[],
            false,
            &vendor_nesting.stack,
            &BTreeSet::new(),
        );
        assert_eq!(
            vendor_default_heads.get("w1").map(String::as_str),
            Some("w1:p7")
        );
        let vendor_quota_heads = group_head_pane_ids(
            &vendor_inventory,
            &[],
            &[],
            true,
            &vendor_nesting.stack,
            &BTreeSet::new(),
        );
        assert_eq!(
            vendor_quota_heads.get("w1").map(String::as_str),
            Some("w1:p10")
        );

        let labels = BTreeMap::from([("w1".to_string(), "ifs".to_string())]);
        let mut head_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut head_desired,
            &head,
            &heads,
            &labels,
            VendorRow::Flat,
            None,
        );
        assert_eq!(
            head_desired.get("quota_group").map(String::as_str),
            Some("ifs")
        );

        let mut sibling_desired = sibling.tokens.clone();
        apply_group_and_icon(
            &mut sibling_desired,
            &sibling,
            &heads,
            &labels,
            VendorRow::Flat,
            None,
        );
        assert!(!sibling_desired.contains_key("quota_group"));
        assert!(
            sibling_desired
                .get("quota_icon")
                .is_some_and(|icon| icon.starts_with(GROUP_MEMBER_INDENT)),
            "member logo must carry the hang-indent: {:?}",
            sibling_desired.get("quota_icon")
        );
        assert!(
            !sibling_desired.contains_key("quota_pad"),
            "stand-alone pad draws a leading · separator"
        );
        assert_eq!(
            group_label_for(&sibling, &heads, &labels),
            None,
            "stale sibling header must clear"
        );
        assert!(
            head_desired
                .get("quota_icon")
                .is_some_and(|icon| !icon.starts_with('\u{200b}')),
            "head logo stays bare; Herdr hang-indents the row"
        );
        assert_eq!(
            format!("{GROUP_MEMBER_INDENT}x").trim(),
            format!("{GROUP_MEMBER_INDENT}x"),
            "indent glued to a glyph must survive Unicode trim"
        );

        // Brand icon colour follows agent_status on `$quota_icon` so a Space
        // head does not hang-indent a later twin one cell to the right.
        let mut working = sibling.clone();
        working.status = AgentStatus::Working;
        let mut working_desired = BTreeMap::from([
            ("quota_icon".to_string(), "stale".to_string()),
            ("quota_icon_done".to_string(), "stale".to_string()),
        ]);
        apply_group_and_icon(
            &mut working_desired,
            &working,
            &heads,
            &labels,
            VendorRow::Flat,
            None,
        );
        assert!(
            working_desired.get("quota_icon").is_some_and(|icon| {
                icon.starts_with(GROUP_MEMBER_INDENT) && icon.contains(crate::icons::WORKING_TAG)
            }),
            "working panes tag the brand icon: {:?}",
            working_desired.get("quota_icon")
        );
        assert!(!working_desired.contains_key("quota_icon_working"));
        assert!(!working_desired.contains_key("quota_icon_done"));

        let mut done = sibling.clone();
        done.status = AgentStatus::Done;
        let mut done_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut done_desired,
            &done,
            &heads,
            &labels,
            VendorRow::Flat,
            None,
        );
        assert!(
            done_desired
                .get("quota_icon")
                .is_some_and(|icon| icon.contains(crate::icons::DONE_TAG)),
            "done panes tag the brand icon: {:?}",
            done_desired.get("quota_icon")
        );
        assert!(!done_desired.contains_key("quota_icon_done"));
        assert!(!done_desired.contains_key("quota_icon_working"));

        let mut head_done = head.clone();
        head_done.status = AgentStatus::Done;
        let mut head_done_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut head_done_desired,
            &head_done,
            &heads,
            &labels,
            VendorRow::Flat,
            None,
        );
        let want_head_done = format!(
            "{}{}",
            crate::icons::for_harness(Harness::Codex),
            crate::icons::DONE_TAG
        );
        assert_eq!(
            head_done_desired.get("quota_icon").map(String::as_str),
            Some(want_head_done.as_str()),
            "Space-head done icon stays on the first identity token"
        );

        // Merely being focused when a turn finishes does not acknowledge it.
        let mut seen = done.clone();
        seen.focused = true;
        let mut seen_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut seen_desired,
            &seen,
            &heads,
            &labels,
            VendorRow::Flat,
            None,
        );
        assert!(
            seen_desired
                .get("quota_icon")
                .is_some_and(|icon| icon.contains(crate::icons::DONE_TAG)),
            "focused completion stays teal until the focus hook acknowledges it"
        );

        // Unfocused sibling finishing must keep teal — not follow the focused
        // pane's yellow→white shortcut.
        let mut other = sibling.clone();
        other.pane_id = "w1:p3".into();
        other.status = AgentStatus::Done;
        other.focused = false;
        let mut other_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut other_desired,
            &other,
            &heads,
            &labels,
            VendorRow::Flat,
            None,
        );
        assert!(
            other_desired
                .get("quota_icon")
                .is_some_and(|icon| icon.contains(crate::icons::DONE_TAG)),
            "unfocused completion keeps teal until that pane is focused"
        );

        // Same-tab: refresh folds unseen into status=Done before publish.
        // A leftover done token on idle must not paint teal by itself —
        // that is how a stale agent-list read restored teal after focus.
        let mut stale_token = other.clone();
        stale_token.status = AgentStatus::Idle;
        stale_token
            .tokens
            .insert("quota_icon_done".to_string(), "teal".to_string());
        let mut stale_desired = BTreeMap::new();
        apply_group_and_icon(
            &mut stale_desired,
            &stale_token,
            &heads,
            &labels,
            VendorRow::Flat,
            None,
        );
        assert!(
            stale_desired.get("quota_icon").is_some_and(|icon| {
                !icon.contains(crate::icons::DONE_TAG) && !icon.contains(crate::icons::WORKING_TAG)
            }),
            "idle + leftover done token must not restore teal"
        );
        assert!(!stale_desired.contains_key("quota_icon_done"));
    }

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
                    workspace_id: "w1".to_string(),
                    cwd: String::new(),
                    title: String::new(),
                    harness: Harness::Codex,
                    session: None,
                    session_summary: String::new(),
                    topic: String::new(),
                    tokens: BTreeMap::new(),
                    status: AgentStatus::Idle,
                    focused: false,
                },
                AgentPane {
                    pane_id: "w1:p2".to_string(),
                    workspace_id: "w1".to_string(),
                    cwd: String::new(),
                    title: String::new(),
                    harness: Harness::Claude,
                    session: None,
                    session_summary: String::new(),
                    topic: String::new(),
                    tokens: BTreeMap::new(),
                    status: AgentStatus::Idle,
                    focused: false,
                },
                AgentPane {
                    pane_id: "w1:p4".to_string(),
                    workspace_id: "w1".to_string(),
                    cwd: String::new(),
                    title: String::new(),
                    harness: Harness::OpenCode,
                    session: None,
                    session_summary: String::new(),
                    topic: String::new(),
                    tokens: BTreeMap::new(),
                    status: AgentStatus::Idle,
                    focused: false,
                },
            ]
        );
    }

    /// The watcher republishes working panes from `list_agent_state`. Two
    /// Firstmate Codex workers (hooks disabled, so Herdr reports no session)
    /// kept the account-level model — the rollout Codex wrote last — because
    /// only the refresh pass recovered their sessions. The inventory itself
    /// must carry the recovered session so every publisher agrees.
    #[test]
    fn the_watch_inventory_renders_each_wrapped_codex_pane_with_its_own_model() {
        let value = json!({"result": {"agents": [
            {"pane_id": "w28:p2", "agent": "codex", "agent_status": "working",
             "cwd": "/firstmate/projects/ralph-hermes",
             "foreground_cwd": "/treehouse/ralph-hermes-86f4b1/1/ralph-hermes"},
            {"pane_id": "w29:p2", "agent": "codex", "agent_status": "working",
             "cwd": "/firstmate/projects/eaves-and-ember",
             "foreground_cwd": "/treehouse/eaves-and-ember-717502/1/eaves-and-ember"},
            {"pane_id": "w1:p1", "agent": "codex", "agent_status": "idle",
             "cwd": "/project", "agent_session": {"kind": "id", "value": "hooked"}}
        ]}});
        let mut asked = Vec::new();
        let state = agent_state_from(&value, |panes| {
            attach_codex_sessions_with(
                panes,
                |pane_id| match pane_id {
                    "w28:p2" => Some(1_790_098_615),
                    "w29:p2" => Some(1_790_098_627),
                    _ => panic!("a pane Herdr already identified is never inspected"),
                },
                |candidates| {
                    asked = candidates.to_vec();
                    BTreeMap::from([
                        ("w28:p2".to_string(), "hermes-live".to_string()),
                        ("w29:p2".to_string(), "ember-live".to_string()),
                    ])
                },
            )
        });
        assert_eq!(
            asked,
            vec![
                (
                    "w28:p2".to_string(),
                    "/treehouse/ralph-hermes-86f4b1/1/ralph-hermes".to_string(),
                    1_790_098_615
                ),
                (
                    "w29:p2".to_string(),
                    "/treehouse/eaves-and-ember-717502/1/eaves-and-ember".to_string(),
                    1_790_098_627
                ),
            ]
        );
        assert_eq!(state.working_pane_ids, vec!["w28:p2", "w29:p2"]);

        let mut snapshot = ProviderSnapshot::new(Provider::Codex, Vec::new(), 0);
        // The newest rollout on the account belongs to another session.
        snapshot.model = Some("gpt-5.6-terra".to_string());
        for id in ["hermes-live", "ember-live", "hooked"] {
            snapshot
                .session_models
                .insert(id.to_string(), "gpt-6-astra".to_string());
        }
        for pane in &state.panes {
            let session = pane.session.as_ref().and_then(AgentSession::id);
            let values = MetadataTokens::from_snapshot_for_pane(
                &snapshot,
                0,
                session,
                PercentStyle::default(),
                SidebarShape::default(),
            );
            assert_eq!(
                values.quota_provider_model, "Codex/gpt-6-astra",
                "{}",
                pane.pane_id
            );
        }
    }

    #[test]
    fn foreground_cwd_is_preferred_for_a_wrapped_agent_process() {
        let value = json!({"result": {"agents": [{
            "pane_id": "w1:p1",
            "agent": "codex",
            "cwd": "/project",
            "foreground_cwd": "/treehouse/project"
        }]}});
        let mut panes = Vec::new();
        collect_agent_panes(&value, &mut panes);
        assert_eq!(panes[0].cwd, "/treehouse/project");
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
                SidebarShape::default(),
            );
            assert!(
                desired.len() <= HERDR_TOKEN_REPORT_CAP,
                "{provider:?} would report {} tokens: {:?}",
                desired.len(),
                desired.keys().collect::<Vec<_>>()
            );
            // A monthly window has its own token; it must not ride a weekly one.
            for (name, value) in &desired {
                if name.starts_with("quota_month") {
                    continue;
                }
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
        for agent in ["claude", "codex", "grok", "agy", "opencode", "devin"] {
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
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::from([(String::from("quota_badge"), String::from("[A]"))]),
            status: AgentStatus::Idle,
            focused: false,
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
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
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
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
        };
        let names = metadata_report_names(&pane, &desired);
        assert!(names.len() <= MAX_METADATA_TOKENS);
        assert!(names.contains(&"quota_cache"));
        assert!(names.contains(&"quota_cache_ttl"));
    }

    /// The context row is coloured by the context *left* under `gauges`, on
    /// the windows' own bands, whichever side of the ledger it prints.
    #[test]
    fn gauges_publishes_context_into_the_severity_name_its_headroom_earns() {
        let gauges = SidebarShape::from(crate::cli::SidebarLayout::Gauges);
        for (used, expected) in [
            (31.0, "quota_context_normal"),
            (49.0, "quota_context_normal"),
            (50.0, "quota_context_normal"),
            (51.0, "quota_context_warning"),
            (53.0, "quota_context_warning"),
            (79.0, "quota_context_warning"),
            (80.0, "quota_context_warning"),
            (81.0, "quota_context_danger"),
            (85.0, "quota_context_danger"),
        ] {
            for percent in [PercentStyle::Remaining, PercentStyle::Used] {
                let mut tokens = BTreeMap::new();
                apply_context(
                    &mut tokens,
                    &ContextUsage::new(used).unwrap(),
                    0,
                    RowStyle::new(percent, gauges),
                );
                let published = CONTEXT_TOKEN_NAMES
                    .into_iter()
                    .filter(|name| tokens.contains_key(*name))
                    .collect::<Vec<_>>();
                assert_eq!(
                    published,
                    vec![expected],
                    "context {used} used, {percent:?}"
                );
            }
        }
    }

    /// Only one context name is ever filled, so a severity change or a layout
    /// switch can never leave a pane showing two context rows.
    #[test]
    fn a_context_severity_change_clears_the_name_it_moved_away_from() {
        let gauges = SidebarShape::from(crate::cli::SidebarLayout::Gauges);
        let mut tokens = BTreeMap::new();
        let row = RowStyle::new(PercentStyle::Used, gauges);
        apply_context(&mut tokens, &ContextUsage::new(31.0).unwrap(), 0, row);
        apply_context(&mut tokens, &ContextUsage::new(85.0).unwrap(), 0, row);
        assert!(!tokens.contains_key("quota_context_normal"));
        assert_eq!(
            tokens.get("quota_context_danger").map(String::as_str),
            Some("cx 85%")
        );
        apply_context(
            &mut tokens,
            &ContextUsage::new(85.0).unwrap(),
            0,
            RowStyle::default(),
        );
        assert_eq!(
            tokens.get("quota_context").map(String::as_str),
            Some("context 85%")
        );
        for name in ["quota_context_normal", "quota_context_danger"] {
            assert!(!tokens.contains_key(name), "{name}");
        }
    }

    /// Related cache details share a line when both fields are visible and
    /// the joined text fits the content width; they split again when it does
    /// not. Packed and stacked never fold, and hiding either field keeps the
    /// two tokens apart so the empty Herdr row can still collapse.
    #[test]
    fn gauges_join_cache_and_ttl_when_they_fit_the_content_width() {
        let cache_ttl = || {
            BTreeMap::from([
                ("quota_cache".to_string(), "cache 95.2%".to_string()),
                ("quota_cache_ttl".to_string(), "ttl≈29m".to_string()),
            ])
        };
        let mut wide = cache_ttl();
        fold_cache_row(
            &mut wide,
            RowStyle {
                fields: FieldSet::all(),
                percent: PercentStyle::Remaining,
                shape: SidebarShape::new(SidebarLayout::Gauges, 26),
                pacing: Default::default(),
            },
        );
        assert_eq!(
            wide.get("quota_cache").map(String::as_str),
            Some("cache 95.2% · ttl≈29m")
        );
        assert!(!wide.contains_key("quota_cache_ttl"));

        let mut narrow = cache_ttl();
        fold_cache_row(
            &mut narrow,
            RowStyle {
                fields: FieldSet::all(),
                percent: PercentStyle::Remaining,
                shape: SidebarShape::new(SidebarLayout::Gauges, 18),
                pacing: Default::default(),
            },
        );
        assert_eq!(
            narrow.get("quota_cache").map(String::as_str),
            Some("cache 95.2%")
        );
        assert_eq!(
            narrow.get("quota_cache_ttl").map(String::as_str),
            Some("ttl≈29m")
        );

        let mut cache_only = cache_ttl();
        fold_cache_row(
            &mut cache_only,
            RowStyle {
                fields: FieldSet::parse("cache").unwrap(),
                percent: PercentStyle::Remaining,
                shape: SidebarShape::new(SidebarLayout::Gauges, 26),
                pacing: Default::default(),
            },
        );
        assert_eq!(
            cache_only.get("quota_cache").map(String::as_str),
            Some("cache 95.2%")
        );
        assert!(cache_only.contains_key("quota_cache_ttl"));

        let mut packed = cache_ttl();
        fold_cache_row(
            &mut packed,
            RowStyle::new(
                PercentStyle::Remaining,
                SidebarShape::from(SidebarLayout::Packed),
            ),
        );
        assert_eq!(
            packed.get("quota_cache").map(String::as_str),
            Some("cache 95.2%")
        );
        assert!(packed.contains_key("quota_cache_ttl"));
    }

    /// `no cached` stays on `$quota_cache_state` even when the joined text
    /// would fit. Concatenating it into `$quota_cache` would share the line
    /// and lose the amber warning; gauges puts the two tokens on one row.
    #[test]
    fn gauges_do_not_fold_no_cached_into_the_cache_token() {
        let mut tokens = BTreeMap::from([
            ("quota_cache".to_string(), "cache 70.7%".to_string()),
            ("quota_cache_state".to_string(), "no cached".to_string()),
        ]);
        fold_cache_row(
            &mut tokens,
            RowStyle {
                fields: FieldSet::all(),
                percent: PercentStyle::Remaining,
                shape: SidebarShape::new(SidebarLayout::Gauges, 36),
                pacing: Default::default(),
            },
        );
        assert_eq!(
            tokens.get("quota_cache").map(String::as_str),
            Some("cache 70.7%")
        );
        assert_eq!(
            tokens.get("quota_cache_state").map(String::as_str),
            Some("no cached")
        );
    }

    /// `packed` and `stacked` keep the plain uncoloured name they have always
    /// published, and the used percent they have always printed, whatever the
    /// context value and whichever percent style the windows are drawn with.
    #[test]
    fn packed_and_stacked_keep_publishing_the_plain_context_token() {
        for layout in [
            crate::cli::SidebarLayout::Packed,
            crate::cli::SidebarLayout::Stacked,
        ] {
            for percent in [PercentStyle::Remaining, PercentStyle::Used] {
                let mut tokens = BTreeMap::new();
                apply_context(
                    &mut tokens,
                    &ContextUsage::new(85.0).unwrap(),
                    0,
                    RowStyle::new(percent, SidebarShape::from(layout)),
                );
                assert_eq!(
                    tokens.get("quota_context").map(String::as_str),
                    Some("context 85%"),
                    "{layout:?} {percent:?}"
                );
                for name in [
                    "quota_context_normal",
                    "quota_context_warning",
                    "quota_context_danger",
                ] {
                    assert!(!tokens.contains_key(name), "{layout:?} wrote {name}");
                }
            }
        }
    }

    /// The three context names are three more slots against Herdr's sixteen,
    /// even though at most one of them is ever filled.
    #[test]
    fn a_full_gauges_pane_stays_inside_herdr_metadata_cap() {
        let gauges = SidebarShape::from(crate::cli::SidebarLayout::Gauges);
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::FiveHour,
                    20.0,
                    Some(crate::model::ResetAt::from_unix_seconds(18_000)),
                )
                .unwrap(),
                crate::model::UsageWindow::new(
                    crate::model::WindowKind::Weekly,
                    30.0,
                    Some(crate::model::ResetAt::from_unix_seconds(183_600)),
                )
                .unwrap(),
            ],
            0,
        )
        .with_context(Some(
            crate::model::ContextUsage::new(85.0)
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
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            crate::cli::PercentStyle::default(),
            gauges,
        );
        let desired = desired_tokens(&values, "prompt", gauges);
        assert!(desired.contains_key("quota_context_danger"));
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
        };
        let names = metadata_report_names(&pane, &desired);
        assert!(names.len() <= MAX_METADATA_TOKENS, "{names:?}");
    }

    #[test]
    fn exact_context_without_cache_clears_stale_cache_diagnostics() {
        let mut tokens = BTreeMap::from([
            ("quota_context".to_string(), "context 99%".to_string()),
            ("quota_cache".to_string(), "cache 95.0%".to_string()),
            ("quota_cache_ttl".to_string(), "ttl≈1h".to_string()),
        ]);
        apply_context(
            &mut tokens,
            &ContextUsage::new(12.0).unwrap(),
            0,
            RowStyle::default(),
        );
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
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let mut tokens = desired.clone();
        tokens.insert("quota_summary".to_string(), "old".to_string());
        tokens.insert("quota_status".to_string(), "OK".to_string());
        tokens.insert("quota_badge".to_string(), "[C]".to_string());
        tokens.insert("quota_session".to_string(), "old".to_string());
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Claude,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens,
            status: AgentStatus::Idle,
            focused: false,
        };
        let names = metadata_report_names(&pane, &desired);
        assert!(names.len() <= MAX_METADATA_TOKENS);
        assert!(names.contains(&"quota_cache"));
        assert!(names.contains(&"quota_cache_ttl"));
        assert!(names.contains(&"quota_summary"));
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
    fn a_missing_five_hour_window_without_context_keeps_week_on_the_limits_row() {
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
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        assert!(!desired.contains_key("quota_5h_unknown"));
        assert!(!desired.contains_key("quota_5h_normal"));
        assert!(!desired.contains_key("quota_5h_label"));
        assert!(desired.contains_key("quota_week_normal"));
        assert!(!desired.contains_key("quota_week_inline_normal"));
    }

    #[test]
    fn empty_five_hour_without_context_keeps_week_on_the_limits_row() {
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
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        assert!(!desired.contains_key("quota_5h_normal"));
        assert!(!desired.contains_key("quota_5h_label"));
        assert!(desired.contains_key("quota_week_normal"));
        assert!(!desired.contains_key("quota_week_inline_normal"));
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
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
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
        )
        .with_context(Some(crate::model::ContextUsage::new(42.0).unwrap()));
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let mut tokens = desired.clone();
        tokens.remove("quota_week_inline_normal");
        tokens.insert("quota_week_normal".to_string(), "7d 75% 5d0h".to_string());
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Grok,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens,
            status: AgentStatus::Idle,
            focused: false,
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
        let desired = desired_tokens(
            &MetadataTokens::from_snapshot(&snapshot, 0),
            "prompt",
            SidebarShape::default(),
        );
        let mut tokens = desired.clone();
        tokens.insert("quota_week_inline_normal".to_string(), "7d 99%".to_string());
        let pane = AgentPane {
            pane_id: "w1:p1".to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness: Harness::Codex,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens,
            status: AgentStatus::Idle,
            focused: false,
        };
        assert!(!metadata_matches(&pane.tokens, &desired));
        let names = metadata_report_names(&pane, &desired);
        assert!(names.contains(&"quota_week_inline_normal"));
        assert!(names.contains(&"quota_week_normal"));
        assert!(names.len() <= MAX_METADATA_TOKENS);
    }

    #[test]
    fn a_narrow_sidebar_with_a_known_model_keeps_headroom() {
        let mut tokens = BTreeMap::from([
            ("quota_provider".to_string(), "Grok".to_string()),
            (HEADROOM_TOKEN.to_string(), "086".to_string()),
        ]);
        apply_identity(
            &mut tokens,
            &PaneIdentity {
                provider: "Grok".to_string(),
                model: "grok-4.6".to_string(),
            },
            18,
            VendorRow::Flat,
        );
        assert!(!tokens.contains_key("quota_provider"));
        assert_eq!(tokens.get(HEADROOM_TOKEN).map(String::as_str), Some("086"));
    }

    #[test]
    fn stripping_account_quota_keeps_session_fields() {
        let mut tokens = BTreeMap::from([
            ("quota_provider".to_string(), "Grok".to_string()),
            (
                "quota_provider_model".to_string(),
                "Grok/grok-4.6".to_string(),
            ),
            ("quota_model".to_string(), "grok-4.6".to_string()),
            ("quota_week_inline_normal".to_string(), "7d 87%".to_string()),
            (
                "quota_share_week_inline_normal".to_string(),
                "7d 87%".to_string(),
            ),
            ("quota_context".to_string(), "cx 79%".to_string()),
            (HEADROOM_TOKEN.to_string(), "087".to_string()),
        ]);
        strip_account_quota_tokens(&mut tokens);
        assert!(!tokens.contains_key("quota_week_inline_normal"));
        assert!(!tokens.contains_key("quota_share_week_inline_normal"));
        assert_eq!(
            tokens.get("quota_provider").map(String::as_str),
            Some("Grok")
        );
        assert_eq!(tokens.get(HEADROOM_TOKEN).map(String::as_str), Some("087"));
        assert_eq!(
            tokens.get("quota_model").map(String::as_str),
            Some("grok-4.6")
        );
        assert_eq!(
            tokens.get("quota_provider_model").map(String::as_str),
            Some("Grok/grok-4.6")
        );
    }

    #[test]
    fn nested_share_windows_match_the_snapshot_without_drift() {
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Grok,
            vec![crate::model::UsageWindow::new(
                crate::model::WindowKind::Weekly,
                25.0,
                Some(crate::model::ResetAt::from_unix_seconds(183_600)),
            )
            .unwrap()],
            0,
        )
        .with_context(Some(crate::model::ContextUsage::new(42.0).unwrap()));
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        let shape = SidebarShape::default();
        let mut current = desired_tokens(&values, "", shape);
        promote_shared_quota(&mut current);
        assert!(
            current.keys().any(|name| name.starts_with("quota_share_")),
            "{current:?}"
        );
        assert!(
            !quota_rows_have_drifted(&current, &values, shape),
            "share tokens must compare equal to the snapshot's quota_* windows"
        );
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

    #[test]
    fn grok_build_help_rows_are_not_topics() {
        assert_eq!(extract_topic("❯ /login\n", Harness::Grok), None);
        assert_eq!(
            extract_topic(
                "❯ /login                         Log in or re-authenticate with your account\n",
                Harness::Grok
            ),
            None
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
