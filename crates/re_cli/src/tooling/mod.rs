//! Subcommands that need a tooling crate (ADR-0027), compiled only with the
//! `tooling` feature. `mcp.rs` and `agent.rs` belong to WP7 and `editor.rs` to
//! WP8; this module (WP6) declares them and the clap tree uses their frozen
//! `<X>Args` and `run`. `check.rs` (WP6) is `real check` and verify's scan stage; `do_cmd.rs`
//! (WP6) is `real do`; `import.rs` (WP16) is `real import`.

pub mod agent;
pub mod check;
pub mod do_cmd;
pub mod editor;
pub mod import;
pub mod mcp;
