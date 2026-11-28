//! Fail closed on infrastructure errors. A located evaluator error must recur
//! in the private store, with the comparison tree evaluating there afterwards.
use super::*;
use serde::Deserialize;
#[derive(Deserialize)]
struct NixLogLine {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    level: Option<i64>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<i64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    file: Option<serde_json::Value>,
}
/// Strip ANSI SGR color escapes (`\x1b[...m`).
fn strip_sgr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'm' {
                i = j + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}
/// Equivalent to `\bat [^\n]+:[0-9]+:[0-9]+` on ANSI-stripped Nix error messages.
fn has_location(msg: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    for line in msg.lines() {
        let mut search = 0;
        while let Some(rel) = line[search..].find("at ") {
            let pos = search + rel;
            let boundary = pos == 0 || !line[..pos].chars().next_back().is_some_and(is_word);
            if boundary {
                let rest = &line[pos + 3..];
                for (colon, _) in rest.match_indices(':') {
                    if colon == 0 {
                        continue;
                    }
                    let after = &rest[colon + 1..];
                    let d1 = after.bytes().take_while(u8::is_ascii_digit).count();
                    if d1 > 0 && after.as_bytes().get(d1) == Some(&b':') {
                        let d2 = after[d1 + 1..]
                            .bytes()
                            .take_while(u8::is_ascii_digit)
                            .count();
                        if d2 > 0 {
                            return true;
                        }
                    }
                }
            }
            search = pos + 3;
        }
    }
    false
}
pub fn errors(log: &[u8]) {
    if let Ok(text) = std::str::from_utf8(log) {
        for line in text.lines() {
            if let Some(j) = line.strip_prefix("@nix ") {
                if let Ok(entry) = serde_json::from_str::<NixLogLine>(j)
                    && entry.action.as_deref() == Some("msg")
                    && entry.level.is_some_and(|l| l <= 1)
                    && let Some(msg) = entry.msg
                    && !msg.is_empty()
                {
                    println!("{msg}");
                }
            } else {
                println!("{line}");
            }
        }
    }
}
pub fn own(code: i32, log: &[u8], first: bool) -> Result<()> {
    if code != 1 {
        return Err(err(format!(
            "it ended with {code}, not an evaluation's failure, so it can't tell"
        )));
    }
    // Invalid individual JSON lines are ignored, just as nix's text diagnostics
    // are. Only an actual structured, located error can count. On the second
    // try, a realise (102) counts as a fetch: it reached for a path its store
    // wasn't given, an input the first try couldn't fetch, say, whose error
    // is the fetch's.
    let entries: Vec<NixLogLine> = String::from_utf8_lossy(log)
        .lines()
        .filter_map(|line| line.strip_prefix("@nix "))
        .filter_map(|j| serde_json::from_str::<NixLogLine>(j).ok())
        .collect();
    if !first {
        let fetched: Vec<&str> = entries
            .iter()
            .filter(|e| {
                e.action.as_deref() == Some("start")
                    && matches!(
                        e.kind,
                        Some(100 | 101 | 102 | 105 | 108 | 109 | 110 | 111 | 112)
                    )
            })
            .map(|e| match e.text.as_deref() {
                Some("") => "a path its store hadn't",
                Some(t) => t,
                None => "",
            })
            .collect();
        if !fetched.is_empty() {
            return Err(err(format!(
                "it built or fetched on the way ({}), so it can't tell",
                fetched.join("; ")
            )));
        }
    }
    let located = entries.iter().any(|e| {
        e.action.as_deref() == Some("msg")
            && e.level == Some(0)
            && (e.file.as_ref().is_some_and(|f| !f.is_null())
                || e.msg
                    .as_deref()
                    .is_some_and(|m| has_location(&strip_sgr(m))))
    });
    if located {
        Ok(())
    } else {
        Err(err(
            "nix doesn't place its error in any code, so it can't tell",
        ))
    }
}
pub fn builder_failed(drv: &str, log: &[u8]) -> bool {
    let mut named = false;
    for line in String::from_utf8_lossy(log).lines() {
        let old = format!("builder for '{drv}' failed with exit code ");
        let candidate = line.find(&old).map(|i| &line[i + old.len()..]).or_else(|| {
            if named {
                line.trim_start()
                    .strip_prefix("Reason: builder failed with exit code ")
            } else {
                None
            }
        });
        if candidate
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|s| s.parse::<u32>().ok())
            .is_some_and(|code| (1..128).contains(&code) && code != 124)
        {
            return true;
        }
        named = line.contains(&format!("Cannot build '{drv}'."));
    }
    false
}
/// Whether the build whose log this is timed out on `drv` itself. Nix says a
/// build timed out without naming it, and names what waited on one that did
/// as failing for a dependency, so it counts only where every build the
/// log started was `drv`'s.
pub fn timed_out(drv: &str, log: &[u8]) -> bool {
    let text = plain(&String::from_utf8_lossy(log));
    let mut started = text
        .lines()
        .filter_map(|l| l.strip_prefix("building '"))
        .map(|l| l.split('\'').next().unwrap_or_default())
        .peekable();
    started.peek().is_some() && started.all(|d| d == drv) && text.lines().any(nix_timed_out)
}
/// Nix's own line for a build it stopped at its `timeout` or
/// `max-silent-time`, and nothing else that says "timed out" (a download's,
/// say).
fn nix_timed_out(line: &str) -> bool {
    line.strip_prefix("error: timed out after ")
        .and_then(|l| l.split_once(' '))
        .is_some_and(|(n, rest)| {
            !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
                && matches!(rest, "seconds" | "seconds of silence")
        })
}
/// The text without its colours.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
fn is_fetch(json: &[u8]) -> Result<bool> {
    let map = derivations_map(json)?;
    if map.len() != 1 {
        return Err(err("not one derivation"));
    }
    let drv = map.values().next().unwrap();
    let outputs = drv
        .get("outputs")
        .and_then(serde_json::Value::as_object)
        .filter(|o| !o.is_empty() && o.values().all(serde_json::Value::is_object))
        .ok_or_else(|| err("no outputs"))?;
    Ok(outputs
        .values()
        .any(|o| o.as_object().is_some_and(|m| m.contains_key("hash"))))
}
pub fn fetch(drv: &str) -> Result<bool> {
    let json = checked(cmd("nix", &["derivation", "show", drv]))?;
    is_fetch(&json)
}
fn drv_inputs(json: &[u8], store: &str) -> Result<Vec<String>> {
    let map = derivations_map(json)?;
    let drv = map.values().next().ok_or_else(|| err("no derivation"))?;
    let inputs = drv
        .get("inputs")
        .and_then(|i| i.get("drvs"))
        .or_else(|| drv.get("inputDrvs"))
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| err("no inputs"))?;
    let mut out = Vec::new();
    for (k, v) in inputs {
        let outputs: Vec<&str> = if let Some(arr) = v.as_array() {
            arr.iter()
                .map(|item| item.as_str().ok_or_else(|| err("non-string output")))
                .collect::<Result<_>>()?
        } else if let Some(arr) = v.get("outputs").and_then(serde_json::Value::as_array) {
            arr.iter()
                .map(|item| item.as_str().ok_or_else(|| err("non-string output")))
                .collect::<Result<_>>()?
        } else {
            return Err(err("malformed input outputs"));
        };
        if !outputs.is_empty() {
            let path = if k.contains('/') {
                k.clone()
            } else {
                format!("{store}/{k}")
            };
            out.push(format!("{path}^{}", outputs.join(",")));
        }
    }
    Ok(out)
}
pub fn inputs(drv: &str) -> Result<Vec<String>> {
    let json = checked(cmd("nix", &["derivation", "show", drv]))?;
    let store = s(Path::new(drv).parent().ok_or_else(|| err("no store"))?);
    drv_inputs(&json, store)
}
#[cfg(test)]
mod tests {
    const DRV: &str = "/nix/store/5gw3q31q4dwc0b0i22np93g5sg2gjbg6-alone.drv";
    /// Nix's words (2.34) for a build that timed out on its own, and for one
    /// whose dependency did, or whose builder says it.
    #[test]
    fn only_its_own_build_timing_out_counts() {
        let own = format!(
            "this derivation will be built:\n  {DRV}\nbuilding '\u{1b}[35;1m{DRV}\u{1b}[0m'...\nalone> ok\n\u{1b}[31;1merror:\u{1b}[0m timed out after \u{1b}[35;1m1800\u{1b}[0m seconds\n"
        );
        assert!(super::timed_out(DRV, own.as_bytes()));
        let dep = format!(
            "building '/nix/store/nryj-hang.drv'...\nhang> hi\nerror: timed out after 1800 seconds\nerror: Cannot build '{DRV}'.\n       Reason: 1 dependency failed.\n"
        );
        assert!(!super::timed_out(DRV, dep.as_bytes()));
        let builder = format!(
            "building '{DRV}'...\nalone> error: timed out after 3 seconds\nerror: Cannot build '{DRV}'.\n       Reason: builder failed with exit code 101.\n"
        );
        assert!(!super::timed_out(DRV, builder.as_bytes()));
        assert!(!super::timed_out(
            DRV,
            b"error: timed out after 1800 seconds\n"
        ));
        let download = format!(
            "building '{DRV}'...\nerror: unable to download 'https://cache.example/x.narinfo': timed out after 30000 milliseconds\n"
        );
        assert!(!super::timed_out(DRV, download.as_bytes()));
        let silence =
            format!("building '{DRV}'...\nerror: timed out after 3600 seconds of silence\n");
        assert!(super::timed_out(DRV, silence.as_bytes()));
    }

    #[test]
    fn own_classifies_located_unlocated_and_fetch_errors_without_jq() {
        let located = br#"@nix not-json
@nix {"action":"msg","level":0,"msg":"\u001b[31;1merror:\u001b[0m broken\n  at \u001b[1m/flake.nix\u001b[0m:12:3:\n"}
"#;
        assert!(super::own(1, located, true).is_ok());
        assert!(super::own(1, located, false).is_ok());
        let with_file = br#"@nix {"action":"msg","level":0,"file":"/flake.nix","msg":"boom"}"#;
        assert!(super::own(1, with_file, true).is_ok());
        let unlocated =
            br#"@nix {"action":"msg","level":0,"msg":"\u001b[31;1merror:\u001b[0m out of memory"}"#;
        let err = super::own(1, unlocated, true).unwrap_err().to_string();
        assert!(err.contains("doesn't place its error"), "{err}");
        let cat_not_at = br#"@nix {"action":"msg","level":0,"msg":"cat /flake.nix:1:2"}"#;
        assert!(super::own(1, cat_not_at, true).is_err());
        let fetched = br#"@nix {"action":"start","type":102,"text":""}
@nix {"action":"msg","level":0,"msg":"at /flake.nix:1:2"}
"#;
        assert!(super::own(1, fetched, true).is_ok());
        let err = super::own(1, fetched, false).unwrap_err().to_string();
        assert!(err.contains("a path its store hadn't"), "{err}");
    }

    #[test]
    fn fetch_and_inputs_parse_both_derivation_show_formats() {
        let fixed = br#"{"derivations":{"a.drv":{"outputs":{"out":{"hash":"abc"}}}}}"#;
        let normal = br#"{"/nix/store/a.drv":{"outputs":{"out":{"path":"/nix/store/a"}}}}"#;
        assert!(super::is_fetch(fixed).unwrap());
        assert!(!super::is_fetch(normal).unwrap());
        assert!(super::is_fetch(b"{}").is_err());

        let newer = br#"{"derivations":{"a.drv":{"inputs":{"drvs":{"b.drv":{"outputs":["out","dev"]},"c.drv":{"outputs":[]}}}}}}"#;
        assert_eq!(
            super::drv_inputs(newer, "/nix/store").unwrap(),
            ["/nix/store/b.drv^out,dev"]
        );
        let older = br#"{"/nix/store/a.drv":{"inputDrvs":{"/nix/store/b.drv":["out"]}}}"#;
        assert_eq!(
            super::drv_inputs(older, "/nix/store").unwrap(),
            ["/nix/store/b.drv^out"]
        );
        assert!(super::drv_inputs(b"{}", "/nix/store").is_err());
    }
}
