use crate::cache::CacheStore;
use crate::model::{Harness, Provider};
use anyhow::Result;
use std::collections::BTreeMap;

pub use crate::herdr_base::*;
pub(crate) use crate::herdr_base::{plugin_quota_present, quota_rows_have_drifted};

/// Agy statusLine observations are keyed by the Herdr pane id, not by
/// Antigravity's conversation id. Herdr's own `agent_session` remains untouched
/// on the server for resume; only this plugin's local pane copy uses the pane
/// id so quota/model/context lookup cannot cross Agy panes.
fn bind_agy_quota_session(pane: &mut AgentPane) {
    if pane.harness != Harness::Agy {
        return;
    }
    pane.session = Some(AgentSession {
        kind: Some("id".to_string()),
        value: pane.pane_id.clone(),
    });
}

fn bind_agy_quota_sessions(panes: &mut [AgentPane]) {
    for pane in panes {
        bind_agy_quota_session(pane);
    }
}

/// Herdr's own Claude SessionStart integration can be absent, outdated, or
/// unwired into `settings.json` (`herdr integration status` reports this),
/// leaving `agent_session` unset even though this pane's statusLine
/// observation is fresh and correctly keyed under its exact session id. Fall
/// back to the mapping the plugin's own statusLine hook records on every
/// tick (see `CacheStore::save_pane_session`), keyed by `HERDR_PANE_ID` — the
/// same env var Herdr's own integration reads. A session Herdr does report is
/// always kept as-is; this only fills a gap, never overrides one.
fn attach_claude_pane_session(pane: &mut AgentPane, sessions: &BTreeMap<String, String>) {
    if pane.harness != Harness::Claude || pane.session.is_some() {
        return;
    }
    if let Some(session_id) = sessions.get(&pane.pane_id) {
        pane.session = Some(AgentSession {
            kind: Some("id".to_string()),
            value: session_id.clone(),
        });
    }
}

fn attach_claude_pane_sessions(panes: &mut [AgentPane], cache: &CacheStore) {
    if !panes
        .iter()
        .any(|pane| pane.harness == Harness::Claude && pane.session.is_none())
    {
        return;
    }
    let sessions = cache.pane_sessions(Provider::Claude);
    for pane in panes {
        attach_claude_pane_session(pane, &sessions);
    }
}

pub fn list_agent_state(cache: &CacheStore) -> Result<AgentState> {
    let mut state = crate::herdr_base::list_agent_state()?;
    bind_agy_quota_sessions(&mut state.panes);
    attach_claude_pane_sessions(&mut state.panes, cache);
    Ok(state)
}

pub fn list_agent_panes(cache: &CacheStore) -> Result<Vec<AgentPane>> {
    let mut panes = crate::herdr_base::list_agent_panes()?;
    bind_agy_quota_sessions(&mut panes);
    attach_claude_pane_sessions(&mut panes, cache);
    Ok(panes)
}

pub fn find_agent_pane(pane_id: &str, cache: &CacheStore) -> Result<Option<AgentPane>> {
    let mut pane = crate::herdr_base::find_agent_pane(pane_id)?;
    if let Some(pane) = pane.as_mut() {
        bind_agy_quota_session(pane);
        attach_claude_pane_sessions(std::slice::from_mut(pane), cache);
    }
    Ok(pane)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn pane(harness: Harness, pane_id: &str, session: Option<&str>) -> AgentPane {
        AgentPane {
            pane_id: pane_id.to_string(),
            workspace_id: "w1".to_string(),
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
        }
    }

    #[test]
    fn agy_uses_pane_id_only_for_plugin_snapshot_lookup() {
        let mut pane = pane(Harness::Agy, "w1:p7", Some("subagent-conversation"));
        bind_agy_quota_session(&mut pane);
        assert_eq!(
            pane.session.as_ref().and_then(AgentSession::id),
            Some("w1:p7")
        );
    }

    #[test]
    fn other_harness_sessions_are_unchanged() {
        for harness in [
            Harness::Claude,
            Harness::Codex,
            Harness::Grok,
            Harness::OpenCode,
            Harness::Pi,
            Harness::Omp,
            Harness::Devin,
            Harness::Muse,
            Harness::Cursor,
        ] {
            let mut pane = pane(harness, "w1:p7", Some("provider-session"));
            bind_agy_quota_session(&mut pane);
            assert_eq!(
                pane.session.as_ref().and_then(AgentSession::id),
                Some("provider-session"),
                "{harness:?} session changed"
            );
        }
    }

    #[test]
    fn claude_pane_missing_agent_session_resolves_from_self_reported_map() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        cache
            .save_pane_session(Provider::Claude, "w1:p7", "session-abc")
            .unwrap();
        let mut panes = vec![pane(Harness::Claude, "w1:p7", None)];
        attach_claude_pane_sessions(&mut panes, &cache);
        assert_eq!(
            panes[0].session.as_ref().and_then(AgentSession::id),
            Some("session-abc")
        );
    }

    #[test]
    fn claude_pane_session_reported_by_herdr_is_never_overridden() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        cache
            .save_pane_session(Provider::Claude, "w1:p7", "self-reported-session")
            .unwrap();
        let mut panes = vec![pane(
            Harness::Claude,
            "w1:p7",
            Some("herdr-reported-session"),
        )];
        attach_claude_pane_sessions(&mut panes, &cache);
        assert_eq!(
            panes[0].session.as_ref().and_then(AgentSession::id),
            Some("herdr-reported-session")
        );
    }

    #[test]
    fn claude_pane_with_no_self_reported_session_stays_none() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut panes = vec![pane(Harness::Claude, "w1:p7", None)];
        attach_claude_pane_sessions(&mut panes, &cache);
        assert!(panes[0].session.is_none());
    }

    #[test]
    fn self_reported_map_does_not_leak_into_other_panes_or_harnesses() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        cache
            .save_pane_session(Provider::Claude, "w1:p7", "session-for-p7")
            .unwrap();
        let mut panes = vec![
            pane(Harness::Claude, "w1:p9", None),
            pane(Harness::Codex, "w1:p7", None),
        ];
        attach_claude_pane_sessions(&mut panes, &cache);
        assert!(panes[0].session.is_none());
        assert!(panes[1].session.is_none());
    }

    /// End-to-end: Herdr reports no `agent_session` at all (the exact
    /// observed symptom), but the plugin's own statusLine tick already
    /// recorded this pane's session id, and the cached snapshot has fresh
    /// quota windows and context keyed under that same id. Once the pane is
    /// attached, quota_5h/quota_week *and* quota_context must all resolve -
    /// not just the windows.
    #[test]
    fn quota_and_context_resolve_once_a_session_less_pane_is_self_reported() {
        use crate::model::{ContextUsage, ProviderSnapshot, WindowKind};
        use crate::presentation::MetadataTokens;

        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        cache
            .save_pane_session(Provider::Claude, "w1:p7", "s1")
            .unwrap();

        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 900).session_local();
        snapshot.session_windows.insert(
            "s1".to_string(),
            vec![
                crate::model::UsageWindow::new(WindowKind::FiveHour, 40.0, None).unwrap(),
                crate::model::UsageWindow::new(WindowKind::Weekly, 20.0, None).unwrap(),
            ],
        );
        snapshot
            .session_contexts
            .insert("s1".to_string(), ContextUsage::new(12.0).unwrap());

        let mut panes = vec![pane(Harness::Claude, "w1:p7", None)];
        attach_claude_pane_sessions(&mut panes, &cache);
        let session_id = panes[0].session.as_ref().and_then(AgentSession::id);
        assert_eq!(session_id, Some("s1"));

        let values = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            1_000,
            session_id,
            Default::default(),
            Default::default(),
        );
        assert!(values.quota_5h_severity.is_some(), "quota_5h missing");
        assert!(values.quota_week_severity.is_some(), "quota_week missing");
        assert!(
            values.quota_context_severity.is_some(),
            "quota_context missing"
        );
    }
}
