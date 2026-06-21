//! Unified error type for the pipeline. Serializes to a plain string so the
//! frontend always receives a readable message.
use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("PDF error: {0}")]
    Pdf(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Kokoro sidecar error: {0}")]
    Sidecar(String),

    #[error("ffmpeg error: {0}")]
    Ffmpeg(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Other(e.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::Sidecar(e.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
