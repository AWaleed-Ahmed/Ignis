//! Connector-local LLM patch proposals (`propose_patch`, contracts-v1.5.0).
//!
//! The customer's model key and every model call stay inside this connector.
//! raphael-core sends typed incident context; the prompt template lives here.

pub mod config;
