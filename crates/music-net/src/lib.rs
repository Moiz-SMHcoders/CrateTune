//! [`TimedTcp`]: a `TcpStream` whose read and write timeouts are safe to hit all the
//! time, for loops that poll one connection from one thread (read for a few
//! milliseconds, send what is queued, read again).
//!
//! On Linux and macOS these are the socket's own timeouts. Windows can't use them that
//! way: now and then a `recv` that times out through `SO_RCVTIMEO` fails with error 997
//! (`WSA_IO_PENDING`). Measured over loopback with numbered writes: once per ~28 000
//! timeouts on Windows 11 (build 26300), where the process then usually crashed with an
//! access violation or lost bytes, and once per ~880 000 on Windows Server 2022, losing
//! bytes each time. Microsoft warns about this: after such a timeout the socket state is
//! "indeterminate". So on Windows the socket is non-blocking and the wait is `WSAPoll`
//! (the same runs: no error, crash or lost byte).

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

/// A read or write that times out fails with [`io::ErrorKind::WouldBlock`] or
/// [`io::ErrorKind::TimedOut`], and can simply be tried again.
pub struct TimedTcp {
    tcp: TcpStream,
    #[cfg(windows)]
    read_timeout: Option<Duration>,
    #[cfg(windows)]
    write_timeout: Option<Duration>,
}

impl TimedTcp {
    /// Takes over a connected stream (any blocking mode; no timeouts yet).
    pub fn new(tcp: TcpStream) -> io::Result<Self> {
        tcp.set_nonblocking(cfg!(windows))?;
        Ok(Self {
            tcp,
            #[cfg(windows)]
            read_timeout: None,
            #[cfg(windows)]
            write_timeout: None,
        })
    }

    /// `None`: wait as long as it takes.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.read_timeout = timeout;
            Ok(())
        }
        #[cfg(not(windows))]
        self.tcp.set_read_timeout(timeout)
    }

    /// `None`: wait as long as it takes.
    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.write_timeout = timeout;
            Ok(())
        }
        #[cfg(not(windows))]
        self.tcp.set_write_timeout(timeout)
    }

    /// Like [`TcpStream::peek`], with the read timeout.
    pub fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(windows)]
        return windows::retry_when_ready(&self.tcp, windows::READ, self.read_timeout, || {
            self.tcp.peek(buf)
        });
        #[cfg(not(windows))]
        self.tcp.peek(buf)
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.tcp.shutdown(how)
    }
}

impl Read for TimedTcp {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(windows)]
        return windows::retry_when_ready(&self.tcp, windows::READ, self.read_timeout, || {
            (&self.tcp).read(buf)
        });
        #[cfg(not(windows))]
        self.tcp.read(buf)
    }
}

impl Write for TimedTcp {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(windows)]
        return windows::retry_when_ready(&self.tcp, windows::WRITE, self.write_timeout, || {
            (&self.tcp).write(buf)
        });
        #[cfg(not(windows))]
        self.tcp.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::net::TcpStream;
    use std::os::windows::io::AsRawSocket;
    use std::time::Duration;

    pub(super) const READ: i16 = 0x0100; // POLLRDNORM
    pub(super) const WRITE: i16 = 0x0010; // POLLWRNORM

    #[repr(C)]
    struct PollFd {
        fd: usize,
        events: i16,
        revents: i16,
    }

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn WSAPoll(fds: *mut PollFd, count: u32, timeout_ms: i32) -> i32;
    }

    /// `op` on the non-blocking socket; if it would block, waits for `events` and tries
    /// once more (still blocked after the wait: the caller sees `WouldBlock`, a timeout).
    pub(super) fn retry_when_ready<T>(
        tcp: &TcpStream,
        events: i16,
        timeout: Option<Duration>,
        mut op: impl FnMut() -> io::Result<T>,
    ) -> io::Result<T> {
        match op() {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                wait(tcp, events, timeout)?;
                op()
            }
            other => other,
        }
    }

    fn wait(tcp: &TcpStream, events: i16, timeout: Option<Duration>) -> io::Result<()> {
        let mut fd = PollFd {
            fd: tcp.as_raw_socket() as usize,
            events,
            revents: 0,
        };
        let timeout_ms = timeout.map_or(-1, |t| t.as_millis().clamp(1, i32::MAX as u128) as i32);
        // SAFETY: `fd` is one valid entry, alive for the (blocking) call.
        match unsafe { WSAPoll(&mut fd, 1, timeout_ms) } {
            -1 => Err(io::Error::last_os_error()),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::time::Instant;

    use super::*;

    fn pair() -> (TimedTcp, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (TimedTcp::new(client).unwrap(), server)
    }

    fn is_timeout(e: &io::Error) -> bool {
        matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        )
    }

    #[test]
    fn a_quiet_read_times_out_and_can_be_retried() {
        let (mut timed, mut peer) = pair();
        timed
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut buf = [0u8; 8];
        let start = Instant::now();
        let e = timed.read(&mut buf).unwrap_err();
        assert!(is_timeout(&e), "{e:?}");
        assert!(start.elapsed() >= Duration::from_millis(30));
        assert!(is_timeout(&timed.peek(&mut buf).unwrap_err()));

        peer.write_all(b"hello").unwrap();
        timed
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        assert_eq!(timed.peek(&mut buf).unwrap(), 5);
        assert_eq!(timed.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], b"hello");
    }

    #[test]
    fn a_read_wakes_up_as_soon_as_data_arrives() {
        let (mut timed, mut peer) = pair();
        timed
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            peer.write_all(b"x").unwrap();
            peer
        });
        let start = Instant::now();
        assert_eq!(timed.read(&mut [0u8; 8]).unwrap(), 1);
        assert!(start.elapsed() < Duration::from_secs(30));
        drop(writer.join().unwrap());
        // The peer is gone: end of stream, not a timeout.
        assert_eq!(timed.read(&mut [0u8; 8]).unwrap(), 0);
    }

    #[test]
    fn a_full_send_buffer_times_out_and_nothing_is_lost() {
        let (mut timed, mut peer) = pair();
        timed
            .set_write_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let block = vec![7u8; 64 * 1024];
        let mut written = 0usize;
        loop {
            match timed.write(&block) {
                Ok(n) => written += n,
                Err(e) => {
                    assert!(is_timeout(&e), "{e:?}");
                    break;
                }
            }
            assert!(written < 1 << 30, "the send buffer never filled up");
        }
        peer.set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut buf = vec![0u8; 64 * 1024];
        let mut read = 0usize;
        while read < written {
            let n = peer.read(&mut buf).unwrap();
            assert!(buf[..n].iter().all(|&b| b == 7));
            read += n;
        }
        assert_eq!(read, written);
    }
}
