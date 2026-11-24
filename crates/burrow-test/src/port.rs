//! A port on 127.0.0.1 that nothing listens on, held for the test.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::os::fd::OwnedFd;

use rustix::net::{AddressFamily, SocketType};

/// A port on 127.0.0.1 bound but not listening, for as long as this lives:
/// a connection to it is refused, and no other socket is given the port.
///
/// A port bound and dropped is free again at once, so a test running
/// alongside can be given it and answer what was meant to find nobody there.
/// [`ClosedPort::listen`] opens this one where it is.
pub struct ClosedPort {
    socket: OwnedFd,
    addr: SocketAddr,
}

impl ClosedPort {
    pub fn new() -> ClosedPort {
        // Without SO_REUSEADDR, which std's listeners set: no bind of theirs,
        // to port 0 or to this one, shares it.
        let socket = rustix::net::socket(AddressFamily::INET, SocketType::STREAM, None).unwrap();
        rustix::net::bind(&socket, &SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = SocketAddr::try_from(rustix::net::getsockname(&socket).unwrap()).unwrap();
        ClosedPort { socket, addr }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Listening on the port from now on.
    pub fn listen(self) -> TcpListener {
        rustix::net::listen(&self.socket, 128).unwrap();
        TcpListener::from(self.socket)
    }
}

impl Default for ClosedPort {
    fn default() -> ClosedPort {
        ClosedPort::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::TcpStream;

    #[test]
    fn a_closed_port_refuses_and_is_nobody_elses_until_it_listens() {
        let closed = ClosedPort::new();
        let addr = closed.addr();
        let refused = TcpStream::connect(addr).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::ConnectionRefused, "{refused}");
        // What a test running alongside would do to stand up its own server.
        assert!(
            TcpListener::bind(addr).is_err(),
            "another socket took {addr}"
        );
        let listener = closed.listen();
        assert_eq!(listener.local_addr().unwrap(), addr);
        TcpStream::connect(addr).unwrap();
        listener.accept().unwrap();
    }
}
