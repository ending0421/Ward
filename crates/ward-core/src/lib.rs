//! Ward core: the reusable engine behind Spot / Replay / Catch / Form Check.
//!
//! Everything in this crate is a *derived view* over `git + working tree`
//! (design law P1). Nothing here owns truth and nothing here writes code.

pub mod config;
pub mod diff;
pub mod fingerprint;
pub mod fresh;
pub mod git;
pub mod index;
pub mod lang;
pub mod normalize;
pub mod search;
pub mod spec;
pub mod store;
pub mod verify;
