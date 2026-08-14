#![forbid(unsafe_code)]

use std::cell::Cell;
use std::io;
use std::net::{SocketAddr, UdpSocket};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RxFrame<'buffer> {
    bytes: &'buffer [u8],
}

impl<'buffer> RxFrame<'buffer> {
    #[must_use]
    pub const fn from_bytes(bytes: &'buffer [u8]) -> Self {
        Self { bytes }
    }

    #[must_use]
    pub const fn bytes(&self) -> &'buffer [u8] {
        self.bytes
    }
}

#[derive(Debug)]
pub struct FrameLease<'queue> {
    frame: RxFrame<'queue>,
    recycled: &'queue Cell<u64>,
}

impl<'queue> FrameLease<'queue> {
    #[must_use]
    pub const fn frame(&self) -> &RxFrame<'queue> {
        &self.frame
    }
}

impl Drop for FrameLease<'_> {
    fn drop(&mut self) {
        self.recycled.set(self.recycled.get().wrapping_add(1));
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum QueueError {
    Full,
}

#[derive(Debug)]
pub struct InMemoryRx<'frames, const N: usize> {
    frames: [Option<&'frames [u8]>; N],
    head: usize,
    tail: usize,
    recycled: Cell<u64>,
}

impl<'frames, const N: usize> InMemoryRx<'frames, N> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            frames: [None; N],
            head: 0,
            tail: 0,
            recycled: Cell::new(0),
        }
    }

    /// # Errors
    ///
    /// Returns [`QueueError::Full`] rather than overwriting an unread frame.
    pub fn push(&mut self, bytes: &'frames [u8]) -> Result<(), QueueError> {
        if self.tail.wrapping_sub(self.head) == N {
            return Err(QueueError::Full);
        }
        if N == 0 {
            return Err(QueueError::Full);
        }
        let index = self.tail % N;
        self.frames[index] = Some(bytes);
        self.tail = self.tail.wrapping_add(1);
        Ok(())
    }

    pub fn receive(&mut self) -> Option<FrameLease<'_>> {
        if self.head == self.tail || N == 0 {
            return None;
        }
        let index = self.head % N;
        let bytes = self.frames[index].take()?;
        self.head = self.head.wrapping_add(1);
        Some(FrameLease {
            frame: RxFrame::from_bytes(bytes),
            recycled: &self.recycled,
        })
    }

    #[must_use]
    pub fn recycled_count(&self) -> u64 {
        self.recycled.get()
    }
}

impl<const N: usize> Default for InMemoryRx<'_, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Portable baseline that receives directly into one preallocated frame.
///
/// A `UdpFrameLease` mutably borrows this queue, so the buffer cannot be
/// overwritten by another receive until the lease is dropped. Each receive is
/// a syscall and is not an `AF_XDP` or kernel-bypass latency path.
#[derive(Debug)]
pub struct UdpRx<const MTU: usize> {
    socket: UdpSocket,
    buffer: [u8; MTU],
}

impl<const MTU: usize> UdpRx<MTU> {
    /// # Errors
    ///
    /// Returns the operating-system bind error.
    pub fn bind(address: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(address)?;
        Ok(Self {
            socket,
            buffer: [0; MTU],
        })
    }

    /// # Errors
    ///
    /// Returns the operating-system receive error.
    pub fn receive(&mut self) -> io::Result<UdpFrameLease<'_>> {
        let (length, peer) = self.socket.recv_from(&mut self.buffer)?;
        Ok(UdpFrameLease {
            frame: RxFrame::from_bytes(&self.buffer[..length]),
            peer,
        })
    }
}

#[derive(Debug)]
pub struct UdpFrameLease<'queue> {
    frame: RxFrame<'queue>,
    peer: SocketAddr,
}

impl UdpFrameLease<'_> {
    #[must_use]
    pub const fn frame(&self) -> &RxFrame<'_> {
        &self.frame
    }

    #[must_use]
    pub const fn peer(&self) -> SocketAddr {
        self.peer
    }
}

#[derive(Debug)]
pub struct UdpTx {
    socket: UdpSocket,
}

impl UdpTx {
    #[must_use]
    pub const fn new(socket: UdpSocket) -> Self {
        Self { socket }
    }

    /// # Errors
    ///
    /// Returns the operating-system send error.
    pub fn send(&self, destination: SocketAddr, frame: &[u8]) -> io::Result<usize> {
        self.socket.send_to(frame, destination)
    }
}

#[cfg(feature = "af-xdp")]
pub mod af_xdp {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Availability {
        VendorImplementationRequired,
    }
}

#[cfg(feature = "vendor")]
pub mod vendor {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Availability {
        SdkNotLinked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_recycles_only_on_drop() {
        let bytes = [1, 2, 3];
        let mut queue = InMemoryRx::<1>::new();
        assert_eq!(queue.push(&bytes), Ok(()));
        {
            let lease = queue.receive().expect("queued frame");
            assert_eq!(lease.frame().bytes(), bytes);
        }
        assert_eq!(queue.recycled_count(), 1);
    }

    #[test]
    fn full_queue_rejects_without_overwrite() {
        let first = [1];
        let second = [2];
        let mut queue = InMemoryRx::<1>::new();
        assert_eq!(queue.push(&first), Ok(()));
        assert_eq!(queue.push(&second), Err(QueueError::Full));
        assert_eq!(queue.receive().expect("first frame").frame().bytes(), first);
    }
}
