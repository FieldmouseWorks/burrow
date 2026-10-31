//! The kick port's other end: how a program on this machine starts a pass.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// POST to URL (`http://HOST:PORT/kick/HANDLER[/OWNER/REPO]`, the kick
/// port's), with nothing in the body: the handler's pass over that repo, or
/// its full pass. Ok once burrow-events took it; otherwise why not.
pub fn kick(url: &str, timeout: Duration) -> Result<(), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("{url} isn't an http:// URL"))?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_owned()),
    };
    let addr = host
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .next()
        .ok_or_else(|| format!("{host}: no address"))?;
    let mut s = TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    s.write_all(
        format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    let mut answer = Vec::new();
    s.read_to_end(&mut answer).map_err(|e| e.to_string())?;
    let status = String::from_utf8_lossy(&answer);
    let code = status
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("no answer");
    if code.starts_with('2') && code.len() == 3 {
        Ok(())
    } else {
        Err(format!("HTTP {code}"))
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::kick;
    use crate::http::listen;

    // A kick the port takes is Ok; one it turns away, or no port at all,
    // says why.
    #[test]
    fn kicks() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let seen = got.clone();
        let (port, _) = listen(
            0,
            Arc::new(move |req| {
                seen.lock().unwrap().push(req.target.clone());
                if req.target == "/kick/nope" { 404 } else { 202 }
            }),
        )
        .unwrap();
        let t = Duration::from_secs(5);
        assert_eq!(
            kick(&format!("http://127.0.0.1:{port}/kick/autoland/sam/r"), t),
            Ok(())
        );
        assert_eq!(
            kick(&format!("http://127.0.0.1:{port}/kick/nope"), t),
            Err("HTTP 404".into())
        );
        assert_eq!(*got.lock().unwrap(), ["/kick/autoland/sam/r", "/kick/nope"]);
        let closed = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(kick(&format!("http://{closed}/kick/x"), t).is_err());
        assert!(kick("https://127.0.0.1/kick/x", t).is_err());
    }
}
