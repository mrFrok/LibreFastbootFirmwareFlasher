//! Finds official firmware for OPPO, OnePlus and realme phones by asking
//! their update servers. No fastboot, no UI: requests and answers only.
//!
//! The server protocol comes from OplusUpdater (Houvven; RealmeOS fork),
//! GPL-3.0; see `protocol`.

pub mod protocol;
pub mod regions;

pub use protocol::{Answer, Mode, Query};
pub use regions::Region;
