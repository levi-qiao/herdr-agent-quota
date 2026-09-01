pub mod agy;
pub mod claude;
pub mod codex;
pub mod grok;
pub mod omp;
pub mod opencode_go;
pub mod statusline;

use thiserror::Error;

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
