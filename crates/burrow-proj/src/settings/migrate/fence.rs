//! Hold the retired locks until this deployed generation stops. The systemd
//! unit is ready only after migration and both locks have been acquired.
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};

pub(super) fn hold() -> Result<(), String> {
    let path = std::env::var_os("NOTIFY_SOCKET").ok_or("--fence requires NOTIFY_SOCKET")?;
    use std::os::unix::ffi::OsStrExt;
    let path = path.as_bytes();
    let address = if let Some(name) = path.strip_prefix(b"@") {
        SocketAddr::from_abstract_name(name)
    } else {
        SocketAddr::from_pathname(std::ffi::OsStr::from_bytes(path))
    }
    .map_err(|e| e.to_string())?;
    let socket = UnixDatagram::unbound().map_err(|e| e.to_string())?;
    socket
        .send_to_addr(b"READY=1", &address)
        .map_err(|e| e.to_string())?;
    loop {
        std::thread::park();
    }
}
