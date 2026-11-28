//! What a gate that didn't pass signs (#1400): `fail` takes someone saying a
//! check, or the flake, failed; whatever else ended it is the gate's own
//! `error`, with why. That way round, as a gate with no room to write, or
//! killed, can't be counted on to say it broke.
use super::*;
/// The worker's exit when the gate broke; 1 is a check's failure alone.
pub const BROKEN: i32 = 3;
/// From here up an exit says a command was killed or never ran (126, 127,
/// 128 and a signal), so nothing of what it was checking.
pub const STOPPED: i32 = 126;
/// The flake's own failure, or what the gate refuses of it or of how it was
/// called: the same on a second try. Every other error is the gate's.
#[derive(Debug)]
pub struct Failed(String);
impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Failed {}
pub fn failed(s: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(Failed(s.into()))
}
/// The first way the gate broke in a step it went on from (the other
/// builds, the whole-flake fallback): what a failure after it is.
static BROKE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
pub fn broke(e: &(dyn std::error::Error + Send + Sync)) {
    if let Ok(mut first) = BROKE.lock() {
        first.get_or_insert_with(|| e.to_string());
    }
}
/// How a command that judges the flake ended: whether it passed, or an
/// error if it was stopped before it could say.
pub fn passed(program: &str, status: std::process::ExitStatus) -> Result<bool> {
    match exit_code(status) {
        code if code >= STOPPED => Err(err(format!(
            "{program} was killed or couldn't run (exit {code})"
        ))),
        code => Ok(code == 0),
    }
}
/// [`checked`], for a command whose failure is the flake's.
pub fn judged(c: Command) -> Result<Vec<u8>> {
    let program = program(&c);
    let o = output(c)?;
    if o.status.success() {
        return Ok(o.stdout);
    }
    io::stderr().write_all(&o.stderr)?;
    passed(&program, o.status)?;
    Err(failed(format!("{program} failed: {}", o.status)))
}
/// The gate worker's exit, its error said as its last line: the parent's
/// reason when it broke.
pub fn worker(result: Result<bool>) -> i32 {
    let mut own = None;
    match result {
        Ok(true) => return 0,
        Ok(false) => {}
        Err(e) if e.is::<Failed>() => eprintln!("{e}"),
        Err(e) => own = Some(e.to_string()),
    }
    let first = BROKE.lock().ok().and_then(|mut first| first.take());
    match first.or(own) {
        Some(why) => {
            eprintln!("{}", one_line(&why));
            BROKEN
        }
        None => 1,
    }
}
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
/// Where the gate says how it failed, for the steps that sign it or hand
/// it to the job that does: `fail`, or `error` and why, on one line.
fn file() -> PathBuf {
    temp().join("gate-result")
}
pub fn clear() {
    let _ = fs::remove_file(file());
}
/// The gate's exit, from how watching its worker ended, having said which
/// failure it was. An error's reason is an annotation too, which needs no
/// room on this machine's disk and outlasts a lost log.
pub fn settle(watched: Result<Ended>) -> i32 {
    let why = match watched {
        Ok(Ended { code: 0, .. }) => return 0,
        Ok(Ended { code: 1, .. }) => None,
        Ok(Ended { code: BROKEN, last }) => Some(last),
        // Not what it said last: that's some build's, and the reason is
        // signed for good.
        Ok(Ended { code, .. }) => Some(format!("the gate's worker ended with {code}")),
        Err(e) => Some(e.to_string()),
    };
    let mut out = io::stdout();
    let said = match why.as_deref().map(one_line) {
        None => "fail\n".to_owned(),
        Some(why) => {
            let _ = writeln!(
                out,
                "::error title=The gate broke::{}",
                why.replace('%', "%25")
            );
            format!("error {why}\n")
        }
    };
    if let Err(e) = fs::write(file(), said) {
        let _ = writeln!(
            out,
            "couldn't write how the gate failed ({e}), so it's signed as an error"
        );
    }
    1
}
