//! Captain's Log native module (Rust), loaded by Electron's main process.
//! Each submodule is a port of one main-process JS file; the JS versions
//! stay as fallbacks until the move to Tauri.
pub mod gamelog; // gamelog.js
pub mod market;  // market.js
pub mod rsi;     // rsi.js
pub mod store;   // store.js

/// Runs f, turning a Rust panic into a JS error instead of letting it take
/// the whole app down (a panic can't unwind into Electron).
pub(crate) fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(Error::from_reason("internal error in the Rust module")))
}

// Error type: napi's when built for Electron, a plain one for the Tauri app.
#[cfg(feature = "node")]
pub use napi::{Error, Result};
#[cfg(not(feature = "node"))]
pub use plain::{Error, Result};
#[cfg(not(feature = "node"))]
mod plain {
    #[derive(Debug, Clone)]
    pub struct Error { pub reason: String }
    impl Error { pub fn from_reason(r: impl Into<String>) -> Self { Error { reason: r.into() } } }
    impl std::fmt::Display for Error { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { f.write_str(&self.reason) } }
    pub type Result<T> = std::result::Result<T, Error>;
}
