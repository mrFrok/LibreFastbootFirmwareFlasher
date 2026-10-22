//! Finds official firmware for OPPO, OnePlus and realme phones by asking
//! their update servers. No fastboot, no UI: requests and answers only.
//!
//! The server protocol comes from OplusUpdater (Houvven; RealmeOS fork),
//! GPL-3.0; see `protocol`. The search and the reading of answers follow
//! the behaviour of NeFeroN's LinkResolverDownloader.

pub mod build;
pub mod changelog;
pub mod protocol;
pub mod regions;
pub mod search;

pub use build::Build;
pub use protocol::{Answer, Mode, Query, agent};
pub use regions::Region;
pub use search::{Found, Search, search};
