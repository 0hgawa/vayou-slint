use thiserror::Error;

/// Log a discarded `Result`'s error (with a short context tag) instead of
/// swallowing it silently. Many mpv/IO calls are fire-and-forget — we don't act
/// on the failure, but a dropped error should still be diagnosable in the logs
/// rather than vanishing. Replaces the bare `let _ = …;` at those call sites.
pub trait LogErr {
    /// Log at `warn` level if this is an `Err`; do nothing on `Ok`.
    fn log_err(self, context: &str);
}

impl<T, E: std::fmt::Display> LogErr for Result<T, E> {
    fn log_err(self, context: &str) {
        if let Err(e) = self {
            tracing::warn!(error = %e, "{context}");
        }
    }
}

/// Application-level error. Rendered to a string for the UI (toast / panel).
#[derive(Debug, Error)]
pub enum AppError {
    #[error("mpv: {0}")]
    Mpv(#[from] MpvError),

    #[error("IO: {0}")]
    Io(#[from] std::io::Error),

    #[error("config: {0}")]
    Config(String),
}

/// mpv-specific errors with structured context.
#[derive(Debug, Error)]
pub enum MpvError {
    #[error("not initialized")]
    NotInitialized,

    #[error("library not loaded: {0}")]
    LibraryLoad(String),

    #[error("symbol '{name}': {detail}")]
    Symbol { name: String, detail: String },

    #[error("error {code}: {context}")]
    Api { code: i32, context: String },

    #[error("invalid C string")]
    NulString(#[from] std::ffi::NulError),
}

impl MpvError {
    /// Create an API error with context.
    pub fn api(code: i32, context: &str) -> Self {
        Self::Api { code, context: context.to_string() }
    }

    /// Create a symbol-not-found error.
    pub fn symbol(name: &str, detail: impl std::fmt::Display) -> Self {
        Self::Symbol { name: name.to_string(), detail: detail.to_string() }
    }
}
