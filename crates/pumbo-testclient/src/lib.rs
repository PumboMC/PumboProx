//! `pumbo-testclient`: a scripted Minecraft Java client for protocol tests
//! (plan §7.2) and for recording vanilla servers in the data generator (§2.5).

pub mod client;
pub mod player;
pub mod recording;
pub mod session;

pub use client::{Client, ClientError};
pub use player::{ChatKey, JoinOptions, Player};
pub use recording::Recorded;
pub use session::{EncryptionSeen, Ending, SessionOptions, SessionOutcome, run, status};
