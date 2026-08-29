use crate::herdr::AgentPane;
use crate::model::{BillingTarget, Harness, Resolution};
use crate::opencode::{
    classify_opencode, env_go_key_present, lookup_session, read_auth, AuthReadError, OpenCodePaths,
};

/// Attribute a pane to a subscription from local evidence only.
///
/// One function with internal match. Missing or malformed OpenCode evidence
/// is [`Resolution::Indeterminate`]; this never infers a pane from the number
/// of credentials on disk.
pub fn resolve(pane: &AgentPane) -> Resolution {
    match pane.harness {
        Harness::Codex | Harness::Grok | Harness::Claude | Harness::Agy => pane
            .harness
            .billing()
            .map(BillingTarget::original_four)
            .map(Resolution::Subscription)
            .unwrap_or(Resolution::Indeterminate),
        Harness::OpenCode => {
            resolve_opencode(pane.session_id.as_deref(), OpenCodePaths::from_env())
        }
        Harness::Pi | Harness::Omp | Harness::Kimi => Resolution::Indeterminate,
    }
}

fn resolve_opencode(session_id: Option<&str>, paths: Option<OpenCodePaths>) -> Resolution {
    let Some(session_id) = session_id.filter(|id| !id.is_empty()) else {
        return Resolution::Indeterminate;
    };
    let Some(paths) = paths else {
        return Resolution::Indeterminate;
    };
    let lookup = lookup_session(&paths, session_id);
    let auth = read_auth(&paths);
    classify_opencode(
        lookup,
        auth.as_ref().map_err(|_| AuthReadError),
        env_go_key_present(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::AgentPane;
    use crate::model::{CredentialScope, Provider};
    use crate::opencode::{parse_auth_json, AuthReadError, SessionEvidence, SessionLookup};
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    fn pane(harness: Harness, session_id: Option<&str>) -> AgentPane {
        AgentPane {
            pane_id: "w1:p9".to_string(),
            harness,
            session_id: session_id.map(str::to_string),
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
        }
    }

    fn write_opencode(dir: &std::path::Path, auth: &str, rows: &[(&str, &str)]) -> OpenCodePaths {
        let data = dir.join("opencode");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("auth.json"), auth).unwrap();
        crate::opencode::write_fixture_db(&data.join("opencode.db"), rows).unwrap();
        OpenCodePaths::from_dir(data)
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
    fn exact_go_session_is_subscription_in_opencode_store_scope() {
        let auth = parse_auth_json(
            br#"{"opencode-go":{"type":"api","key":"placeholder"},"anthropic":{"type":"api","key":"placeholder"}}"#,
        )
        .unwrap();
        let lookup = SessionLookup::Found(SessionEvidence {
            session_id: "ses_go".to_string(),
            provider_id: Some("opencode-go".to_string()),
            model_id: Some("kimi-k2.5".to_string()),
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
    fn known_payg_backend_with_api_key_is_no_subscription() {
        let auth = parse_auth_json(br#"{"anthropic":{"type":"api","key":"placeholder"}}"#).unwrap();
        let lookup = SessionLookup::Found(SessionEvidence {
            session_id: "ses_payg".to_string(),
            provider_id: Some("anthropic".to_string()),
            model_id: Some("claude-sonnet-4".to_string()),
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
        });
        assert_eq!(
            classify_opencode(go, Ok(&empty), true),
            Resolution::Subscription(BillingTarget::opencode_go())
        );
        let payg = SessionLookup::Found(SessionEvidence {
            session_id: "ses_payg".to_string(),
            provider_id: Some("anthropic".to_string()),
            model_id: None,
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
            resolve_opencode(Some("ses_go"), Some(paths.clone())),
            Resolution::Subscription(BillingTarget::opencode_go())
        );
        assert_eq!(
            resolve_opencode(Some("ses_payg"), Some(paths.clone())),
            Resolution::NoSubscription
        );
        assert_eq!(
            resolve_opencode(Some("ses_absent"), Some(paths)),
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
            resolve_opencode(
                None,
                Some(OpenCodePaths::from_dir(directory.path().join("opencode")))
            ),
            Resolution::Indeterminate
        );
    }
}
