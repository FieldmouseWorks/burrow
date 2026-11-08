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
    // are. Only an actual structured, located error can count.
    let log = quote(&String::from_utf8_lossy(log));
    let why = jq(
        log.as_bytes(),
        &format!(
            r#"[split("\n")[] | select(startswith("@nix ")) | .[5:] | fromjson?] as $log | first(
      ([$log[] | select({} and .action == "start" and (.type | IN(100,101,105,108,109,110,111,112))) | .text]
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
pub fn built(drv: &str) -> Result<bool> {
    let o = output(cmd("nix-store", &["-q", "--outputs", drv]))?;
    if !o.status.success() {
        return Ok(false);
    }
    let outs = String::from_utf8(o.stdout)?;
    let outs: Vec<_> = outs.lines().collect();
    if outs.is_empty() {
        return Ok(false);
    }
    let mut c = cmd("nix-store", &["--check-validity"]);
    c.args(outs);
    Ok(output(c)?.status.success())
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
