//! The one vsock listener. Everything else in the broker is behind `GuestListener`.

use std::io;

use tokio_vsock::{VsockAddr, VsockListener, VsockStream, VMADDR_CID_ANY};

use crate::serve::GuestListener;

/// A vsock listener on `VMADDR_CID_ANY`. The guest CID check is in `serve`.
pub struct Vsock {
    listener: VsockListener,
}

impl Vsock {
    /// Binds the listener. Python: `socket.bind((VMADDR_CID_ANY, PORT))`.
    pub fn bind(port: u16) -> io::Result<Self> {
        let listener = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, u32::from(port)))?;
        Ok(Self { listener })
    }
}

impl GuestListener for Vsock {
    type Stream = VsockStream;

    async fn accept(&mut self) -> io::Result<(VsockStream, u32)> {
        let (stream, addr) = self.listener.accept().await?;
        Ok((stream, addr.cid()))
    }
}
