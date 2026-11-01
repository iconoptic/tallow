//! The tallow host daemon. See docs/plans/2026-10-31-rust-conversion.md.
//!
//! It shares the mesh's frame format with the firmware through
//! `tallow-mesh-core`, so the two cannot drift apart.

#[cfg_attr(not(test), allow(dead_code))]
mod link;

use tallow_mesh_core::wire::VERSION;

fn banner() -> String {
    format!(
        "tallowd {} mesh frame version {VERSION:#04x}",
        env!("CARGO_PKG_VERSION")
    )
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_names_the_frame_version() {
        assert!(banner().contains("0xa1"));
    }
}
