//! Fail closed on infrastructure errors. A located evaluator error must recur
//! in the private store, with the comparison tree evaluating there afterwards.
use super::*;
pub fn errors(log: &[u8]) {
    if let Ok(text) = std::str::from_utf8(log) {
        for line in text.lines() {
            if let Some(j) = line.strip_prefix("@nix ") {
                match jq(
                    j.as_bytes(),
                    "select(.action == \"msg\" and .level <= 1) | .msg",
                ) {
                    Ok(msg) if !msg.is_empty() => println!("{msg}"),
                    _ => (),
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
    let log = quote(&String::from_utf8_lossy(log));
    let why = jq(
        log.as_bytes(),
        &format!(
            r#"[split("\n")[] | select(startswith("@nix ")) | .[5:] | fromjson?] as $log | first(
      ([$log[] | select({} and .action == "start" and (.type | IN(100,101,102,105,108,109,110,111,112))) | if .text == "" then "a path its store hadn't" else .text end]
        | select(length > 0) | "it built or fetched on the way (\(join("; ")))"),
      ([$log[] | select(.action == "msg" and .level == 0)
        | select(.file != null or (.msg | gsub("\u001b\\[[0-9;]*m"; "") | test("\\bat [^\n]+:[0-9]+:[0-9]+")))]
        | select(length == 0) | "nix doesn't place its error in any code"))"#,
            if first { "false" } else { "true" }
        ),
    )?;
    if why.is_empty() {
        Ok(())
    } else {
        Err(err(format!("{why}, so it can't tell")))
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
    started.peek().is_some()
        && started.all(|d| d == drv)
        && text
            .lines()
            .any(|l| l.starts_with("error: ") && l.contains(" timed out after "))
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
pub fn fetch(drv: &str) -> Result<bool> {
    let json = checked(cmd("nix", &["derivation", "show", drv]))?;
    Ok(jq(
        &json,
        r#"(.derivations // .) | to_entries | if length == 1 then .[0].value.outputs else error("not one derivation") end
      | if type == "object" and length > 0 and all(.[]; type == "object") then any(.[]; has("hash")) else error("no outputs") end"#,
    )? == "true")
}
pub fn inputs(drv: &str) -> Result<Vec<String>> {
    let json = checked(cmd("nix", &["derivation", "show", drv]))?;
    let store = s(Path::new(drv).parent().ok_or_else(|| err("no store"))?);
    Ok(lines(&jq(
        &json,
        &format!(
            r#"(.derivations // .) | to_entries[0].value
      | (.inputs.drvs // .inputDrvs // error("no inputs")) | to_entries[]
      | (.value | if type == "array" then . else .outputs end) as $o | select($o | length > 0)
      | "\(if .key | contains("/") then .key else {} + "/" + .key end)^\($o | join(","))""#,
            quote(store)
        ),
    )?))
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
    }
}
