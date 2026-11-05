//! `burrow-heartbeat --site CONFIG`: one watch of a site elsewhere, which is
//! HOST-heartbeat-site-SLUG. While the site answers 2xx its own check gets a
//! success ping, the same way the beat's goes (`send`: the key never leaves
//! this process, the endpoint's trailing slashes go, a ping Healthchecks
//! couldn't take is tried again). When it doesn't, nothing is sent: the
//! check's silence is the alert.

use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;

use crate::{Ping, send};

/// What modules/heartbeat.nix builds in for one site.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
    /// The unit's name, HOST-heartbeat-site-SLUG.
    pub name: String,
    /// The Healthchecks instance's ping endpoint, as the beat's.
    pub url: String,
    /// The site's check's slug in the ping key's project.
    pub check: String,
    /// What gets fetched: the site's URL.
    pub site: String,
}

impl SiteConfig {
    pub fn read(path: &Path) -> Result<SiteConfig, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Fetches `site` once through `agent` (no redirect followed): the status
/// when it answered 2xx and its whole body came, or why it didn't. A
/// redirect isn't an answer, whatever it points at.
pub fn answers(agent: &ureq::Agent, site: &str) -> Result<u16, String> {
    let response = agent.get(site).call().map_err(|e| e.to_string())?;
    let code = response.status().as_u16();
    if !(200..300).contains(&code) {
        return Err(format!("HTTP {code}"));
    }
    io::copy(&mut response.into_body().into_reader(), &mut io::sink())
        .map_err(|e| format!("HTTP {code}, but its body broke off ({e})"))?;
    Ok(code)
}

/// One watch: what it says on stdout, and on stderr a ping that didn't land.
/// Neither a site down nor a lost ping is an error: a failed unit would only
/// be noise in HOST-health, and the check's silence already alerts.
pub struct Said {
    pub out: String,
    pub err: Option<String>,
}

pub fn watch(
    agent: &ureq::Agent,
    cfg: &SiteConfig,
    key: &str,
    pause: &mut dyn FnMut(std::time::Duration),
) -> Said {
    let (check, site) = (&cfg.check, &cfg.site);
    let code = match answers(agent, site) {
        Ok(code) => code,
        Err(why) => {
            return Said {
                out: format!("{check}: {site} didn't answer 2xx ({why}), so no ping"),
                err: None,
            };
        }
    };
    let ping = Ping {
        suffix: "",
        body: format!("{site} answers HTTP {code}"),
    };
    Said {
        out: format!("{check}: {site} answers HTTP {code}"),
        err: send(agent, &cfg.url, key, check, &ping, pause)
            .err()
            .map(|e| format!("{check}: the ping to {check} didn't land ({e})")),
    }
}
