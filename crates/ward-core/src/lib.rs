//! Ward core: the reusable engine behind Spot / Replay / Catch / Form Check.
//!
//! Everything in this crate is a *derived view* over `git + working tree`
//! (design law P1). Nothing here owns truth and nothing here writes code.

pub mod config;
pub mod lang;
