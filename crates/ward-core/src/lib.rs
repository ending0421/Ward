//! Ward core: the reusable engine behind Spot / Replay / Catch / Form Check.
//!
//! Everything in this crate is a *derived view* over `git + working tree`
//! (design law P1). Nothing here owns truth and nothing here writes code.

pub mod attribution;
pub mod calibrate;
pub mod cluster;
pub mod compat;
pub mod config;
pub mod context;
pub mod daemon;
pub mod diff;
pub mod doctor;
pub mod embedding;
pub mod fingerprint;
pub mod fresh;
pub mod git;
pub mod index;
pub mod infer;
pub mod intent;
pub mod label;
pub mod lang;
pub mod llm;
pub mod narrate;
pub mod normalize;
pub mod report;
pub mod search;
pub mod spec;
pub mod stats;
pub mod store;
pub mod verify;
