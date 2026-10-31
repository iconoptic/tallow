//! The tallow node mesh, with no radio and no OS in it.
//!
//! Everything here works on bytes and a clock the caller supplies, so routing,
//! authentication, and failover run under `cargo test` on the host. The firmware
//! in `../node` hands it received frames and sends what it asks for.
//!
//! Every node forwards. Each node picks its own parent from beacons and moves
//! when that parent goes quiet. Sightings travel up to whichever node the host
//! is plugged into. Commands flood back down. Frames carry records, not IP, so
//! a node needs a few KB of state rather than queues of 1,500-byte packets.
#![cfg_attr(not(test), no_std)]

pub mod ad;
pub mod mesh;
pub mod names;
pub mod sightings;
pub mod wire;

pub use mesh::{Dest, Event, Mesh, Tx, NO_RANK};
pub use names::Names;
pub use sightings::{Kind, Sightings};
pub use wire::{Frame, FrameKind, MAX_FRAME, MAX_PAYLOAD};
