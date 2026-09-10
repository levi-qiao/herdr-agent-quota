pub mod agy;
pub mod claude;
pub mod codex;
pub mod devin;
pub mod grok;
pub mod muse;
pub mod omp;
pub mod opencode_go;
pub mod statusline;

use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) fn credential_id(key: &str) -> String {
    format!("key:{:x}", Sha256::digest(key.trim().as_bytes()))
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider credentials are unavailable")]
    MissingCredentials,
    #[error("provider quota is unavailable: {0}")]
    Unavailable(String),
    #[error("provider response is not supported: {0}")]
    UnsupportedResponse(String),
    #[error("provider request failed: {0}")]
    Request(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, ProviderSnapshot};
    #[test]
    fn all_direct_collectors_reject_unknown_or_other_account_snapshots() {
        for provider in [
            Provider::Codex,
            Provider::Grok,
            Provider::Devin,
            Provider::Muse,
            Provider::OpenCodeGo,
        ] {
            let mut cached = ProviderSnapshot::new(provider, vec![], 100);
            assert!(!cached.usable_for_account(Some("new"), Some(1)));
            cached.account_id = Some("old".into());
            assert!(!cached.usable_for_account(Some("new"), Some(1)));
            assert!(cached.usable_for_account(Some("old"), Some(200)));
        }
        let a = credential_id("key-a");
        assert_ne!(a, credential_id("key-b"));
        assert!(!a.contains("key-a"));
    }
}
