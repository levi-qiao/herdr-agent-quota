//! Conservative local resolution for the omp coding-agent harness.
//!
//! omp is a fork of Pi. The transcript it writes is still the JSONL v3 shape
//! `src/pi.rs` reads, so the session parser is shared. Everything else moved:
//! credentials live in `<agent dir>/agent.db` instead of `auth.json`, and the
//! model catalog lives in `models.db` instead of `models-store.json`.
//!
//! `agent.db` is never opened: it holds live OAuth tokens, and both the
//! account identity and the quota are available from `omp usage --json`, a
//! documented CLI surface that answers from omp's own five-minute usage cache
//! instead of hitting the provider on every call. `models.db` holds nothing
//! secret and is read read-only, for one thing the CLI cannot give cheaply:
//! the context window of the pane's model.

use crate::model::{ContextUsage, Provider, Resolution};
use crate::pi::{context_usage, lookup_session_in, DetailedSessionLookup, SessionEvidence};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Exact provider lookup in omp's model catalog. Never a full-table scan.
const MODELS_FOR_PROVIDER: &str = "SELECT models FROM model_cache WHERE provider_id = ?1 LIMIT 1";

/// Where an omp pane keeps its state.
///
/// Derived from the absolute session path Herdr reports, never from this
/// process's environment: a plugin action runs in Herdr's environment, so a
/// pane started with `PI_CONFIG_DIR` or `--profile` would otherwise be read
/// against the wrong agent directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmpPaths {
    pub agent_dir: PathBuf,
    pub sessions: PathBuf,
}

impl OmpPaths {
    pub fn models_db(&self) -> PathBuf {
        self.agent_dir.join("models.db")
    }

    /// Recover the agent directory from `<agent dir>/sessions/**/<file>.jsonl`.
    ///
    /// Subagent transcripts nest under their parent, so the sessions component
    /// is found by walking up rather than by a fixed depth.
    pub fn from_session_path(session_path: &Path) -> Option<Self> {
        let mut current = session_path.parent()?;
        loop {
            if current.file_name().and_then(|name| name.to_str()) == Some("sessions") {
                let agent_dir = current.parent()?.to_path_buf();
                return Some(Self {
                    agent_dir,
                    sessions: current.to_path_buf(),
                });
            }
            current = current.parent()?;
        }
    }
}

/// What the transcript proves about the account paying for a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmpEvidence {
    pub paths: OmpPaths,
    /// omp's provider id (`anthropic`, `openai-codex`, `xai`, …).
    pub provider_id: String,
    /// `credential_pin` hash of the serving account, when omp recorded one.
    pub account_pin: Option<String>,
}

pub(crate) struct OmpRoute {
    pub resolution: Resolution,
    pub session: Option<SessionEvidence>,
    pub context: Option<ContextUsage>,
    pub evidence: Option<OmpEvidence>,
}

fn indeterminate() -> OmpRoute {
    OmpRoute {
        resolution: Resolution::Indeterminate,
        session: None,
        context: None,
        evidence: None,
    }
}

/// Every safe omp provider id is collected through omp's own usage layer.
/// Provider-specific compatibility belongs to omp, not this plugin.
pub fn billing_for_provider(provider_id: &str) -> Option<Provider> {
    (!provider_id.is_empty()
        && provider_id.len() <= 128
        && !provider_id.chars().any(char::is_control))
    .then_some(Provider::Omp)
}

/// Context window for the session's model, from omp's own catalog.
///
/// `models.db` is a cache omp rebuilds from each provider's model list, so it
/// is read-only evidence here: a missing row, an unexpected shape, or a
/// database omp is mid-write on all yield `None`, and the pane simply shows no
/// context percentage. A user-configured override of `contextWindow` is not
/// visible in this table, so a heavily customized catalog can read slightly
/// off; the quota rows, which matter more, never come from here.
pub fn context_window(paths: &OmpPaths, session: &SessionEvidence) -> Option<u64> {
    let model_id = session.model_id.as_deref()?;
    let connection =
        Connection::open_with_flags(paths.models_db(), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let models: String = connection
        .query_row(MODELS_FOR_PROVIDER, [&session.provider_id], |row| {
            row.get(0)
        })
        .ok()?;
    serde_json::from_str::<Vec<Value>>(&models)
        .ok()?
        .into_iter()
        .find(|model| model.get("id").and_then(Value::as_str) == Some(model_id))
        .and_then(|model| model.get("contextWindow").and_then(Value::as_u64))
        .filter(|window| *window > 0)
}

pub(crate) fn resolve_with_session(
    session_path: Option<&str>,
    context_window: impl FnOnce(&OmpPaths, &SessionEvidence) -> Option<u64>,
) -> OmpRoute {
    let Some(session_path) = session_path.filter(|path| !path.is_empty()) else {
        return indeterminate();
    };
    let session_path = Path::new(session_path);
    let Some(paths) = OmpPaths::from_session_path(session_path) else {
        return indeterminate();
    };
    let DetailedSessionLookup::Found(parsed) = lookup_session_in(&paths.sessions, session_path)
    else {
        return indeterminate();
    };
    let session = parsed.evidence.clone();
    // A transcript whose last assistant turn was served by another provider is
    // not evidence for the model_change that follows it.
    if parsed.message_provider_id.as_deref() != Some(&session.provider_id) {
        return OmpRoute {
            resolution: Resolution::Indeterminate,
            session: Some(session),
            context: None,
            evidence: None,
        };
    }
    let context = parsed
        .context_tokens
        .zip(context_window(&paths, &session))
        .and_then(|(tokens, window)| context_usage(&parsed, tokens, window));
    let evidence = OmpEvidence {
        paths,
        provider_id: session.provider_id.clone(),
        account_pin: parsed.credential_pin.clone(),
    };
    // Which subscription is paying, and whether there is one at all, is a
    // question for the credential pool, not the transcript. `refresh` asks
    // omp's generic usage layer about this exact provider only.
    let resolution = match billing_for_provider(&session.provider_id) {
        Some(_) => Resolution::Subscription(crate::model::BillingTarget::omp(&session.provider_id)),
        None => Resolution::Indeterminate,
    };
    OmpRoute {
        resolution,
        session: Some(session),
        context,
        evidence: Some(evidence),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_directory_is_recovered_from_a_nested_session_path() {
        let paths = OmpPaths::from_session_path(Path::new(
            "/home/u/.omp/agent/sessions/-home-u-code/2026-09-01_abcd1234.jsonl",
        ))
        .expect("paths");
        assert_eq!(paths.agent_dir, Path::new("/home/u/.omp/agent"));
        assert_eq!(paths.sessions, Path::new("/home/u/.omp/agent/sessions"));
    }

    /// Subagent transcripts nest one directory deeper under their parent.
    #[test]
    fn a_subagent_session_resolves_to_the_same_agent_directory() {
        let paths = OmpPaths::from_session_path(Path::new(
            "/home/u/.omp/agent/sessions/-home-u-code/2026-09-01_abcd1234/sub01.jsonl",
        ))
        .expect("paths");
        assert_eq!(paths.agent_dir, Path::new("/home/u/.omp/agent"));
    }

    #[test]
    fn a_path_outside_a_sessions_directory_resolves_to_nothing() {
        assert_eq!(
            OmpPaths::from_session_path(Path::new("/home/u/.omp/agent/agent.db")),
            None
        );
    }

    /// The catalog read is exact: the right provider's row, the right model in
    /// it, and nothing at all when either is missing.
    #[test]
    fn the_context_window_comes_from_the_models_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let paths = OmpPaths {
            agent_dir: dir.path().to_path_buf(),
            sessions: dir.path().join("sessions"),
        };
        let connection = Connection::open(paths.models_db()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE model_cache (provider_id TEXT PRIMARY KEY, models TEXT NOT NULL);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO model_cache (provider_id, models) VALUES (?1, ?2)",
                rusqlite::params![
                    "anthropic",
                    r#"[{"id":"model-a","contextWindow":200000},{"id":"model-b"}]"#
                ],
            )
            .unwrap();
        let evidence = |model: &str| SessionEvidence {
            provider_id: "anthropic".to_string(),
            model_id: Some(model.to_string()),
        };
        assert_eq!(context_window(&paths, &evidence("model-a")), Some(200_000));
        // A catalog entry without a window is not a zero.
        assert_eq!(context_window(&paths, &evidence("model-b")), None);
        assert_eq!(context_window(&paths, &evidence("model-c")), None);
        assert_eq!(
            context_window(
                &paths,
                &SessionEvidence {
                    provider_id: "xai".to_string(),
                    model_id: Some("model-a".to_string()),
                }
            ),
            None
        );
    }

    /// A pane whose omp has never built a catalog still resolves; it just
    /// shows no context percentage.
    #[test]
    fn a_missing_catalog_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = OmpPaths {
            agent_dir: dir.path().to_path_buf(),
            sessions: dir.path().join("sessions"),
        };
        assert_eq!(
            context_window(
                &paths,
                &SessionEvidence {
                    provider_id: "anthropic".to_string(),
                    model_id: Some("model-a".to_string()),
                }
            ),
            None
        );
    }

    #[test]
    fn every_omp_provider_routes_through_omps_usage_layer() {
        for provider_id in [
            "anthropic",
            "openai-codex",
            "xai-oauth",
            "google-antigravity",
            "openrouter",
        ] {
            assert_eq!(billing_for_provider(provider_id), Some(Provider::Omp));
        }
    }
}
