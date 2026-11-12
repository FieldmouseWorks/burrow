//! The one outbound HTTP agent: every burrow program that calls a service
//! over HTTPS builds its ureq agent here, so a change to the policy (a proxy
//! rule, a user agent, a TLS setting) lands once. The workspace's
//! `clippy.toml` bars `ureq::Agent::config_builder` everywhere else.

use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig};

/// How the agent reaches the network.
#[derive(Debug, Clone, Default)]
pub enum Proxy {
    /// Whatever the environment says (ureq's default).
    #[default]
    Env,
    /// Straight, whatever the environment says.
    Direct,
    /// Through this proxy.
    Via(ureq::Proxy),
}

impl From<Option<ureq::Proxy>> for Proxy {
    fn from(p: Option<ureq::Proxy>) -> Self {
        p.map_or(Proxy::Direct, Proxy::Via)
    }
}

/// What an agent does beyond the shared policy: the machine's own trust
/// (its CA bundle), a global timeout, and a limit on redirects followed.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub proxy: Proxy,
    pub user_agent: Option<String>,
    /// Open a connection of its own for each request.
    pub fresh_connections: bool,
    /// A 4xx or 5xx is an error rather than an answer to read.
    pub status_as_error: bool,
}

/// The agent: `timeout` for a whole request, `max_redirects` followed (0
/// follows none, as curl without `-L`), every status an answer to read.
pub fn agent(timeout: Duration, max_redirects: u32) -> ureq::Agent {
    agent_with(timeout, max_redirects, Options::default())
}

/// [`agent`], with `options`.
#[allow(clippy::disallowed_methods)] // the one place an agent is built
pub fn agent_with(timeout: Duration, max_redirects: u32, options: Options) -> ureq::Agent {
    let tls = TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    let mut b = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(timeout))
        .max_redirects(max_redirects)
        .http_status_as_error(options.status_as_error);
    b = match options.proxy {
        Proxy::Env => b,
        Proxy::Direct => b.proxy(None),
        Proxy::Via(p) => b.proxy(Some(p)),
    };
    if let Some(ua) = options.user_agent {
        b = b.user_agent(ua);
    }
    if options.fresh_connections {
        b = b.max_idle_connections(0);
    }
    b.build().into()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    // One answer served; the agent reads a 302 as an answer and follows none.
    #[test]
    fn reads_a_redirect_rather_than_following_it() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0; 1024];
            let _ = s.read(&mut buf);
            s.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let a = agent_with(
            Duration::from_secs(5),
            0,
            Options {
                proxy: Proxy::Direct,
                ..Options::default()
            },
        );
        let r = a.get(format!("http://127.0.0.1:{port}/")).call().unwrap();
        assert_eq!(r.status().as_u16(), 302);
    }
}
