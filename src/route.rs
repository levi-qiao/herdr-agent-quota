use crate::herdr::{AgentPane, PaneIdentity};
use crate::model::{BillingTarget, ContextUsage, Harness, Resolution};
use crate::opencode::{
    classify_opencode, env_go_key_present, lookup_session, model_context_window, read_auth,
    AuthReadError, OpenCodePaths, SessionEvidence, SessionLookup,
};
use crate::pi::PiPaths;
use crate::providers::codex;

/// Attribute a pane to a subscription from local evidence only.
///
/// One function with internal harness-specific readers. Missing or malformed
/// OpenCode/Pi evidence is [`Resolution::Indeterminate`]; this never infers a
/// pane from the number of credentials on disk.
pub fn resolve(pane: &AgentPane) -> Resolution {
    resolve_with_identity(pane).resolution
}

pub struct ResolvedPane {
    pub resolution: Resolution,
    pub identity: Option<PaneIdentity>,
    pub context: Option<ContextUsage>,
    /// Present only for omp panes. The omp-scoped billing target says which
    /// subscription is paying; this says which of omp's accounts, and where to
    /// ask omp about it.
    pub omp: Option<crate::omp::OmpEvidence>,
}

pub fn resolve_with_identity(pane: &AgentPane) -> ResolvedPane {
    let resolution = match pane.harness {
        Harness::Codex | Harness::Grok | Harness::Claude | Harness::Agy => pane
            .harness
            .billing()
            .map(BillingTarget::original_four)
            .map(Resolution::Subscription)
            .unwrap_or(Resolution::Indeterminate),
        Harness::OpenCode => {
            return resolve_opencode_with_identity(
                pane.session.as_ref().and_then(|session| session.id()),
                OpenCodePaths::from_env(),
            )
        }
        Harness::Pi => {
            return resolve_pi_with_identity(
                pane.session.as_ref(),
                PiPaths::from_env(),
                codex::current_account_id,
            )
        }
        Harness::Omp => {
            return resolve_omp_with_identity(
                pane.session.as_ref().and_then(|session| session.path()),
            )
        }
    };
    ResolvedPane {
        resolution,
        identity: None,
        context: None,
        omp: None,
    }
}

fn resolve_omp_with_identity(session_path: Option<&str>) -> ResolvedPane {
    let route = crate::omp::resolve_with_session(session_path, crate::omp::context_window);
    ResolvedPane {
        resolution: route.resolution,
        // omp inherited Pi's provider ids, so one mapping serves both.
        identity: route.session.as_ref().and_then(pi_identity),
        context: route.context,
        omp: route.evidence,
    }
}

fn resolve_pi_with_identity(
    session: Option<&crate::herdr::AgentSession>,
    paths: Option<PiPaths>,
    canonical_codex_account_id: impl FnOnce() -> Option<String>,
) -> ResolvedPane {
    let route = crate::pi::resolve_with_session(
        session.and_then(|session| session.path()),
        paths,
        canonical_codex_account_id,
    );
    ResolvedPane {
        resolution: route.resolution,
        identity: route.session.as_ref().and_then(pi_identity),
        context: route.context,
        omp: None,
    }
}

/// Display identity for the Pi-family harnesses.
///
/// Shared with omp, which inherited Pi's provider ids and added its own
/// auth-scoped spellings (`xai-oauth` for the SuperGrok login,
/// `google-antigravity`). Names the sidebar already has a colour and a column
/// width for are used where they mean the same subscription; anything else is
/// shown as the harness spells it.
fn pi_identity(session: &crate::pi::SessionEvidence) -> Option<PaneIdentity> {
    let provider = match session.provider_id.as_str() {
        "openai-codex" => "Codex".to_string(),
        "xai" | "xai-oauth" => "Grok".to_string(),
        "anthropic" => "Claude".to_string(),
        "google-antigravity" => "Agy".to_string(),
        value => safe_identity_part(value)?,
    };
    let model = match session.model_id.as_deref() {
        Some(value) => safe_identity_part(value)?,
        None => String::new(),
    };
    Some(PaneIdentity { provider, model })
}

fn safe_identity_part(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control))
        .then(|| value.to_string())
}

fn resolve_opencode_with_identity(
    session_id: Option<&str>,
    paths: Option<OpenCodePaths>,
) -> ResolvedPane {
    let Some(session_id) = session_id.filter(|id| !id.is_empty()) else {
        return indeterminate_pane();
    };
    let Some(paths) = paths else {
        return indeterminate_pane();
    };
    let lookup = lookup_session(&paths, session_id);
    let session = match &lookup {
        SessionLookup::Found(session) => Some(session),
        SessionLookup::Missing | SessionLookup::Unreadable => None,
    };
    let identity = session.and_then(opencode_identity);
    let context = session.and_then(|session| opencode_context(&paths, session));
    let auth = read_auth(&paths);
    let resolution = classify_opencode(
        lookup,
        auth.as_ref().map_err(|_| AuthReadError),
        env_go_key_present(),
    );
    ResolvedPane {
        resolution,
        identity,
        context,
        omp: None,
    }
}

fn indeterminate_pane() -> ResolvedPane {
    ResolvedPane {
        resolution: Resolution::Indeterminate,
        identity: None,
        context: None,
        omp: None,
    }
}

fn opencode_identity(session: &SessionEvidence) -> Option<PaneIdentity> {
    let provider = match session.provider_id.as_deref()? {
        "opencode" => "OpenCode".to_string(),
        "opencode-go" => "OpenCode Go".to_string(),
        value => safe_identity_part(value)?,
    };
    let model = match session.model_id.as_deref() {
        Some(value) => safe_identity_part(value)?,
        None => String::new(),
    };
    Some(PaneIdentity { provider, model })
}

fn opencode_context(paths: &OpenCodePaths, session: &SessionEvidence) -> Option<ContextUsage> {
    let provider_id = session.provider_id.as_deref()?;
    let model_id = session.model_id.as_deref()?;
    let context_tokens = session.context_tokens?;
    let context_window = model_context_window(paths, provider_id, model_id)?;
    let used_percent = (context_tokens as f64 / context_window as f64 * 100.0).clamp(0.0, 100.0);
    ContextUsage::new(used_percent).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::AgentPane;
    use crate::model::{CredentialScope, Provider};
    use crate::opencode::{parse_auth_json, AuthReadError, SessionEvidence, SessionLookup};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn pane(harness: Harness, session_id: Option<&str>) -> AgentPane {
        AgentPane {
            pane_id: "w1:p9".to_string(),
            harness,
            session: session_id.map(|value| crate::herdr::AgentSession {
                kind: Some("id".to_string()),
                value: value.to_string(),
            }),
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
        }
    }

    fn pi_fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi")
            .join(name)
    }

    fn write_opencode(dir: &std::path::Path, auth: &str, rows: &[(&str, &str)]) -> OpenCodePaths {
        let data = dir.join("opencode");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("auth.json"), auth).unwrap();
        crate::opencode::write_fixture_db(&data.join("opencode.db"), rows).unwrap();
        OpenCodePaths::from_dir(data)
    }

    /// An omp transcript is copied into a real `<agent dir>/sessions` tree so
    /// the containment check and the agent-directory walk are both exercised.
    fn omp_session(dir: &std::path::Path, fixture: &str) -> String {
        let sessions = dir.join(".omp/agent/sessions/-workspace");
        fs::create_dir_all(&sessions).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/omp")
            .join(fixture);
        // omp names a transcript `<timestamp>_<session id>.jsonl`, and the
        // reader checks the id against the header before trusting the file.
        let id = std::fs::read_to_string(&source)
            .unwrap()
            .lines()
            .find_map(|line| {
                let entry: serde_json::Value = serde_json::from_str(line).ok()?;
                (entry.get("type")?.as_str()? == "session")
                    .then(|| entry.get("id")?.as_str().map(str::to_string))
                    .flatten()
            })
            .expect("the fixture has a session header");
        let destination = sessions.join(format!("2099-01-01_{id}.jsonl"));
        fs::copy(source, &destination).unwrap();
        destination.to_string_lossy().into_owned()
    }

    fn omp_pane(session_path: &str) -> AgentPane {
        let mut pane = pane(Harness::Omp, Some(session_path));
        pane.session.as_mut().unwrap().kind = Some("path".to_string());
        pane
    }

    /// The omp route is scoped to omp's own credential store: it names the
    /// subscription that pays, and the account the transcript pinned, without
    /// ever borrowing the canonical Claude snapshot.
    #[test]
    fn an_omp_pane_resolves_to_its_own_credential_scope() {
        let dir = tempdir().unwrap();
        let path = omp_session(dir.path(), "session-anthropic.jsonl");
        // Through the harness dispatch, so an omp pane's path-kind session is
        // what actually reaches the reader.
        let resolved = resolve_with_identity(&omp_pane(&path));
        assert_eq!(
            resolved.resolution,
            Resolution::Subscription(BillingTarget::omp(Provider::Claude))
        );
        let identity = resolved.identity.expect("identity");
        assert_eq!(identity.provider, "Claude");
        assert_eq!(identity.model, "model-a");
        let evidence = resolved.omp.expect("evidence");
        assert_eq!(evidence.provider_id, "anthropic");
        assert_eq!(evidence.account_pin.as_deref(), Some("pin-account-one"));
        assert_eq!(evidence.paths.agent_dir, dir.path().join(".omp/agent"));
        // No models.db in the fixture tree yet, so there is no window to divide
        // by and no context percentage is invented.
        assert_eq!(resolved.context, None);

        // With the catalog in place the same transcript reports its context:
        // omp's authoritative 500 context tokens against a 200k window.
        write_omp_catalog(&dir.path().join(".omp/agent/models.db"));
        let context = resolve_omp_with_identity(Some(&path))
            .context
            .expect("context");
        assert!((context.used_percent - 0.25).abs() < 1e-9);
        let cache = context.cache.expect("cache");
        assert_eq!(cache.ttl_seconds, Some(60 * 60));
    }

    fn write_omp_catalog(path: &std::path::Path) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE model_cache (provider_id TEXT PRIMARY KEY, models TEXT NOT NULL);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO model_cache (provider_id, models) VALUES (?1, ?2)",
                rusqlite::params!["anthropic", r#"[{"id":"model-a","contextWindow":200000}]"#],
            )
            .unwrap();
    }

    /// The shape of this fixture is copied from a live omp v18 transcript
    /// (content stripped): the padded `title` header with no id, the
    /// `provider/modelId` selector on `model_change`, `xai-oauth` as the
    /// provider, and two `credential_pin` entries. Every one of those is a
    /// difference from Pi that would otherwise read as "unreadable session".
    #[test]
    fn a_live_shaped_omp_transcript_resolves_to_grok() {
        let dir = tempdir().unwrap();
        let path = omp_session(dir.path(), "session-xai-oauth.jsonl");
        let resolved = resolve_with_identity(&omp_pane(&path));
        assert_eq!(
            resolved.resolution,
            Resolution::Subscription(BillingTarget::omp(Provider::Grok))
        );
        let identity = resolved.identity.expect("identity");
        assert_eq!(identity.provider, "Grok");
        assert_eq!(identity.model, "grok-4.6");
        let evidence = resolved.omp.expect("evidence");
        assert_eq!(evidence.provider_id, "xai-oauth");
        // The later of the two pins, and the one recorded for this provider.
        assert_eq!(
            evidence.account_pin.as_deref(),
            Some("bd751891daabc13cfac0194c5ee2078650a9ff08ba1292460627f4d7a0f86e15")
        );
    }

    /// A provider this plugin has no collector for still gets its identity
    /// row; what it must not get is somebody else's quota.
    #[test]
    fn an_unmapped_omp_provider_carries_identity_without_a_subscription() {
        let dir = tempdir().unwrap();
        let path = omp_session(dir.path(), "session-openrouter.jsonl");
        let resolved = resolve_omp_with_identity(Some(&path));
        assert_eq!(resolved.resolution, Resolution::Indeterminate);
        assert_eq!(
            resolved.identity.map(|identity| identity.provider),
            Some("openrouter".to_string())
        );
    }

    /// A path outside the agent directory that named it is not evidence.
    #[test]
    fn an_omp_session_outside_its_sessions_tree_resolves_to_nothing() {
        let dir = tempdir().unwrap();
        let stray = dir.path().join("session.jsonl");
        fs::write(&stray, "{}\n").unwrap();
        let resolved = resolve_omp_with_identity(Some(&stray.to_string_lossy()));
        assert_eq!(resolved.resolution, Resolution::Indeterminate);
        assert!(resolved.omp.is_none());
    }

    /// An omp pane routed to Claude must never read the canonical Claude
    /// snapshot, or a Pro seat in omp would display the Max seat's quota.
    #[test]
    fn an_omp_target_caches_apart_from_the_canonical_collector() {
        let omp = BillingTarget::omp(Provider::Claude);
        let canonical = BillingTarget::original_four(Provider::Claude);
        assert_ne!(omp.cache_identity(), canonical.cache_identity());
        assert_eq!(omp.credential_scope, CredentialScope::OMP_STORE);
        assert_eq!(omp.original_provider(), Some(Provider::Claude));
    }

    #[test]
    fn original_four_panes_resolve_to_canonical_targets() {
        for (harness, provider) in [
            (Harness::Claude, Provider::Claude),
            (Harness::Codex, Provider::Codex),
            (Harness::Grok, Provider::Grok),
            (Harness::Agy, Provider::Agy),
        ] {
            assert_eq!(
                resolve(&pane(harness, Some("thread-1"))),
                Resolution::Subscription(BillingTarget::original_four(provider))
            );
        }
    }

    #[test]
    fn pi_path_route_reuses_only_a_proved_canonical_codex_scope() {
        let directory = tempdir().unwrap();
        let agent = directory.path().join("agent");
        let sessions = directory.path().join("sessions/project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        fs::copy(pi_fixture("auth-matching.json"), agent.join("auth.json")).unwrap();
        let session_path = sessions.join("2026-08-29T00-00-00-000Z_session-codex.jsonl");
        fs::copy(pi_fixture("session-codex.jsonl"), &session_path).unwrap();
        let paths = PiPaths::from_dirs(agent, directory.path().join("sessions"));
        let session = crate::herdr::AgentSession {
            kind: Some("path".to_string()),
            value: session_path.to_string_lossy().into_owned(),
        };

        let resolved = resolve_pi_with_identity(Some(&session), Some(paths.clone()), || {
            Some("account-same".to_string())
        });
        assert_eq!(
            resolved
                .identity
                .as_ref()
                .map(|identity| (identity.provider.as_str(), identity.model.as_str())),
            Some(("Codex", "model-b"))
        );
        let resolution = resolved.resolution;
        assert_eq!(
            resolution,
            Resolution::Subscription(BillingTarget::original_four(Provider::Codex))
        );
        let Resolution::Subscription(target) = resolution else {
            panic!("expected canonical Codex route")
        };
        assert_eq!(target.credential_scope, CredentialScope::CANONICAL);
        assert_eq!(target.cache_identity(), Provider::Codex.source());

        assert_eq!(
            resolve_pi_with_identity(Some(&session), Some(paths), || {
                Some("different-account".to_string())
            })
            .resolution,
            Resolution::Indeterminate
        );
    }

    #[test]
    fn pi_rejects_an_id_shaped_session_reference() {
        let session = crate::herdr::AgentSession {
            kind: Some("id".to_string()),
            value: "session-codex".to_string(),
        };
        assert_eq!(
            resolve_pi_with_identity(Some(&session), None, || {
                Some("account-same".to_string())
            })
            .resolution,
            Resolution::Indeterminate
        );
    }

    #[test]
    fn exact_go_session_is_subscription_in_opencode_store_scope() {
        let auth = parse_auth_json(
            br#"{"opencode-go":{"type":"api","key":"placeholder"},"anthropic":{"type":"api","key":"placeholder"}}"#,
        )
        .unwrap();
        let lookup = SessionLookup::Found(SessionEvidence {
            session_id: "ses_go".to_string(),
            provider_id: Some("opencode-go".to_string()),
            model_id: Some("kimi-k2.5".to_string()),
            context_tokens: None,
        });
        let resolution = classify_opencode(lookup, Ok(&auth), false);
        assert_eq!(
            resolution,
            Resolution::Subscription(BillingTarget::opencode_go())
        );
        let Resolution::Subscription(target) = resolution else {
            panic!("expected subscription");
        };
        assert_eq!(target.billing, Provider::OpenCodeGo);
        assert_eq!(target.credential_scope, CredentialScope::OPENCODE_STORE);
        assert_eq!(target.cache_identity(), "opencode-go.opencode-store");
    }

    #[test]
    fn exact_opencode_session_exposes_identity_and_context_without_a_subscription() {
        let directory = tempdir().unwrap();
        let paths = write_opencode(
            directory.path(),
            r#"{}"#,
            &[(
                "ses_free",
                r#"{"role":"assistant","providerID":"opencode","modelID":"big-pickle","tokens":{"input":11424,"output":10,"reasoning":0,"cache":{"read":0,"write":0}}}"#,
            )],
        );
        fs::write(
            &paths.models,
            br#"{"opencode":{"models":{"big-pickle":{"limit":{"context":200000}}}}}"#,
        )
        .unwrap();

        let resolved = resolve_opencode_with_identity(Some("ses_free"), Some(paths));
        assert_eq!(resolved.resolution, Resolution::Indeterminate);
        assert_eq!(
            resolved
                .identity
                .as_ref()
                .map(|identity| (identity.provider.as_str(), identity.model.as_str())),
            Some(("OpenCode", "big-pickle"))
        );
        assert_eq!(
            resolved
                .context
                .as_ref()
                .map(|context| context.used_percent),
            Some(5.717)
        );
    }

    #[test]
    fn known_payg_backend_with_api_key_is_no_subscription() {
        let auth = parse_auth_json(br#"{"anthropic":{"type":"api","key":"placeholder"}}"#).unwrap();
        let lookup = SessionLookup::Found(SessionEvidence {
            session_id: "ses_payg".to_string(),
            provider_id: Some("anthropic".to_string()),
            model_id: Some("claude-sonnet-4".to_string()),
            context_tokens: None,
        });
        assert_eq!(
            classify_opencode(lookup, Ok(&auth), false),
            Resolution::NoSubscription
        );
    }

    #[test]
    fn any_keyed_backend_is_no_subscription_without_a_provider_name_list() {
        // models.dev carries 200+ OpenCode backends and adds more over time.
        // Classification comes from the session's own credential, so a backend
        // nobody enumerated still resolves correctly.
        for provider_id in [
            "togetherai",
            "fireworks-ai",
            "moonshotai",
            "a-backend-added-tomorrow",
        ] {
            let auth = parse_auth_json(
                format!(r#"{{"{provider_id}":{{"type":"api","key":"placeholder"}}}}"#).as_bytes(),
            )
            .unwrap();
            let lookup = SessionLookup::Found(SessionEvidence {
                session_id: "ses_payg".to_string(),
                provider_id: Some(provider_id.to_string()),
                model_id: None,
                context_tokens: None,
            });
            assert_eq!(
                classify_opencode(lookup, Ok(&auth), false),
                Resolution::NoSubscription,
                "{provider_id}"
            );
        }
    }

    #[test]
    fn an_oauth_backend_is_preserved_rather_than_cleared() {
        // A subscription login this plugin cannot read yet must not have its
        // pane metadata cleared as if it were confirmed pay-as-you-go.
        let auth = parse_auth_json(br#"{"github-copilot":{"type":"oauth"}}"#).unwrap();
        let lookup = SessionLookup::Found(SessionEvidence {
            session_id: "ses_oauth".to_string(),
            provider_id: Some("github-copilot".to_string()),
            model_id: None,
            context_tokens: None,
        });
        assert_eq!(
            classify_opencode(lookup, Ok(&auth), false),
            Resolution::Indeterminate
        );
    }

    #[test]
    fn missing_session_is_indeterminate_even_with_exactly_one_credential() {
        let auth =
            parse_auth_json(br#"{"opencode-go":{"type":"api","key":"placeholder"}}"#).unwrap();
        assert!(auth.get("opencode-go").is_some());
        assert!(auth.get("anthropic").is_none());
        assert_eq!(
            classify_opencode(SessionLookup::Missing, Ok(&auth), false),
            Resolution::Indeterminate
        );
        assert_eq!(
            classify_opencode(SessionLookup::Unreadable, Ok(&auth), false),
            Resolution::Indeterminate
        );
    }

    #[test]
    fn malformed_auth_or_db_is_indeterminate() {
        let lookup = SessionLookup::Found(SessionEvidence {
            session_id: "ses_go".to_string(),
            provider_id: Some("opencode-go".to_string()),
            model_id: None,
            context_tokens: None,
        });
        assert_eq!(
            classify_opencode(lookup.clone(), Err(AuthReadError), false),
            Resolution::Indeterminate
        );
        assert_eq!(
            classify_opencode(SessionLookup::Unreadable, Err(AuthReadError), true),
            Resolution::Indeterminate
        );
    }

    #[test]
    fn env_go_key_is_only_evidence_for_an_approved_go_route() {
        let empty = parse_auth_json(br#"{}"#).unwrap();
        let go = SessionLookup::Found(SessionEvidence {
            session_id: "ses_go".to_string(),
            provider_id: Some("opencode-go".to_string()),
            model_id: None,
            context_tokens: None,
        });
        assert_eq!(
            classify_opencode(go, Ok(&empty), true),
            Resolution::Subscription(BillingTarget::opencode_go())
        );
        let payg = SessionLookup::Found(SessionEvidence {
            session_id: "ses_payg".to_string(),
            provider_id: Some("anthropic".to_string()),
            model_id: None,
            context_tokens: None,
        });
        assert_eq!(
            classify_opencode(payg, Ok(&empty), true),
            Resolution::Indeterminate
        );
    }

    #[test]
    fn one_disk_credential_does_not_attribute_a_different_backend() {
        let auth =
            parse_auth_json(br#"{"opencode-go":{"type":"api","key":"placeholder"}}"#).unwrap();
        let lookup = SessionLookup::Found(SessionEvidence {
            session_id: "ses_payg".to_string(),
            provider_id: Some("anthropic".to_string()),
            model_id: None,
            context_tokens: None,
        });
        assert_eq!(
            classify_opencode(lookup, Ok(&auth), false),
            Resolution::Indeterminate
        );
    }

    #[test]
    fn opencode_paths_resolve_go_and_payg_from_local_files() {
        let directory = tempdir().unwrap();
        let paths = write_opencode(
            directory.path(),
            r#"{"opencode-go":{"type":"api","key":"placeholder"},"anthropic":{"type":"api","key":"placeholder"}}"#,
            &[
                (
                    "ses_go",
                    r#"{"role":"assistant","providerID":"opencode-go","modelID":"kimi-k2.5"}"#,
                ),
                (
                    "ses_payg",
                    r#"{"role":"assistant","providerID":"anthropic","modelID":"sonnet"}"#,
                ),
            ],
        );
        assert_eq!(
            resolve_opencode_with_identity(Some("ses_go"), Some(paths.clone())).resolution,
            Resolution::Subscription(BillingTarget::opencode_go())
        );
        assert_eq!(
            resolve_opencode_with_identity(Some("ses_payg"), Some(paths.clone())).resolution,
            Resolution::NoSubscription
        );
        assert_eq!(
            resolve_opencode_with_identity(Some("ses_absent"), Some(paths)).resolution,
            Resolution::Indeterminate
        );
    }

    #[test]
    fn pane_without_session_id_is_never_guessed_from_credentials() {
        let directory = tempdir().unwrap();
        let _paths = write_opencode(
            directory.path(),
            r#"{"opencode-go":{"type":"api","key":"placeholder"}}"#,
            &[(
                "ses_go",
                r#"{"role":"assistant","providerID":"opencode-go","modelID":"kimi-k2.5"}"#,
            )],
        );
        assert_eq!(
            resolve_opencode_with_identity(
                None,
                Some(OpenCodePaths::from_dir(directory.path().join("opencode")))
            )
            .resolution,
            Resolution::Indeterminate
        );
    }
}
