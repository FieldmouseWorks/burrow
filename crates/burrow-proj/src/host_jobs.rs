//! Host job operations through a per-turn socket, never host filesystem access.
//! Only the ordinary inspection and recovery CLI is accepted. The host keeps
//! its executable, configuration, credentials and job admission authority.
use crate::{Fail, config::Config, job::Job, scratch};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

pub(crate) const MAX_REQUEST: u64 = 65536;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
// JSON escaping can expand each output byte sixfold.
const MAX_RESPONSE: u64 = (MAX_OUTPUT * 12 + 1024) as u64;

pub(crate) fn socket(cfg: &Config, job: &Job, n: &str) -> PathBuf {
    scratch::dir(cfg, job.id).join(format!("host-jobs-{n}.sock"))
}

/// Commands which must use the host's view when run inside a job. Other
/// commands (await/ask/done in particular) keep their local declarations.
pub fn remote(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some(
            "status"
                | "state"
                | "lifecycle"
                | "log"
                | "reply"
                | "stop"
                | "withdraw"
                | "list"
                | "find"
        )
    )
}

fn number(s: &str) -> bool {
    !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit())
}
fn id(s: &str) -> bool {
    // The launcher retains punctuation, whitespace and Unicode, and prefixes
    // dot/option-like project names. Reserve hidden host state and refuse
    // path traversal/options without imposing another naming policy.
    !s.is_empty() && !s.starts_with(['.', '-']) && !s.contains(['/', '\0'])
}
fn validate(args: &[String]) -> Result<(), String> {
    let words: Vec<_> = args.iter().map(String::as_str).collect();
    let valid = match words.as_slice() {
        ["status" | "list"] => true,
        ["find", key] => crate::cmd::key_name(key),
        ["status", n] => *n == "--line" || number(n),
        ["state", "--schema" | "--json"] => true,
        ["state", "--json", n] => number(n),
        ["state" | "lifecycle" | "stop" | "withdraw", target] => id(target),
        ["log", target, rest @ ..] if id(target) => {
            let mut rest = rest;
            let mut valid = true;
            while !rest.is_empty() {
                rest = match rest {
                    ["--raw", tail @ ..] => tail,
                    ["--turn", n, tail @ ..] if number(n) => tail,
                    _ => {
                        valid = false;
                        break;
                    }
                };
            }
            valid
        }
        ["reply", rest @ ..] => {
            let mut rest = rest;
            loop {
                rest = match rest {
                    ["--first", tail @ ..] => tail,
                    ["--cut-off", n, tail @ ..] if number(n) => tail,
                    _ => break,
                };
            }
            matches!(rest, [target, text] if id(target) && !text.is_empty())
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("host jobs: unsupported command or invalid arguments".into())
    }
}

#[derive(Serialize, Deserialize)]
struct Response {
    code: u8,
    stdout: String,
    stderr: String,
}

pub fn client(path: &Path, args: &[String]) -> Result<(), Fail> {
    validate(args).map_err(|e| Fail::new(2, e))?;
    let request = serde_json::to_vec(args).expect("host job request");
    if request.len() > MAX_REQUEST as usize {
        return Err(Fail::new(2, "host jobs: request exceeds 65536 bytes"));
    }
    let response = crate::probe::request(path, &request, MAX_RESPONSE)
        .and_then(|s| {
            serde_json::from_str::<Result<Response, String>>(&s).map_err(|e| e.to_string())
        })
        .and_then(|r| r)
        .map_err(|e| Fail::new(1, format!("can't reach host jobs: {e}")))?;
    let _ = io::stdout().lock().write_all(response.stdout.as_bytes());
    let _ = io::stderr().lock().write_all(response.stderr.as_bytes());
    if response.code == 0 {
        Ok(())
    } else {
        Err(Fail::quiet(response.code))
    }
}

pub(crate) fn answer(cfg: &Config, request: &str) -> String {
    let result = (|| -> Result<Response, String> {
        let args: Vec<String> = serde_json::from_str(request).map_err(|e| e.to_string())?;
        validate(&args)?;
        let mut command = Command::new(&cfg.me);
        command
            // A machinery continuation is not an owner's answer. Retain
            // open questions just as the watchdog's native reply does.
            .arg(if args[0] == "reply" {
                "_host-reply"
            } else {
                &args[0]
            })
            .args(&args[1..])
            .env("PROJ_CONFIG", &cfg.path)
            .env("PROJ_SELF", &cfg.me)
            .env_remove("PROJ_JOB")
            .env_remove("PROJ_HOST_JOBS_SOCKET")
            .env_remove("PROJ_PROBE_SOCKET");
        let output = burrow_process::output(
            &mut command,
            None,
            Instant::now() + Duration::from_secs(90),
            MAX_OUTPUT,
        )?;
        let code = output
            .status
            .code()
            .and_then(|c| c.try_into().ok())
            .unwrap_or(1);
        Ok(Response {
            code,
            stdout: scrub_child_output(
                &String::from_utf8_lossy(&output.stdout),
                output_contract(&args),
                output.status.success(),
            )?,
            stderr: scrub_child_output(
                &String::from_utf8_lossy(&output.stderr),
                if output_contract(&args) == Output::Records {
                    Output::Records
                } else {
                    Output::Literal
                },
                output.status.success(),
            )?,
        })
    })();
    serde_json::to_string(&result).expect("host job response")
}

/// A failed child may stop before completing its document. Keep its native
/// verdict and diagnostics; successful children must meet their contract.
fn scrub_child_output(text: &str, mut output: Output, success: bool) -> Result<String, String> {
    if !success
        && output == Output::Json
        && serde_json::from_str::<serde_json::Value>(text).is_err()
    {
        // Incomplete JSON can conceal field boundaries behind escapes or
        // nested objects. Preserve the verdict and stderr, but fail closed
        // for that document; plain failure diagnostics remain readable.
        if text.trim_start().starts_with(['{', '[', '"']) {
            return Ok("[redacted incomplete JSON]\n".into());
        }
        output = Output::Literal;
    }
    match scrub_output(text, output) {
        Ok(clean) => Ok(clean),
        Err(_) if !success => Ok("[redacted]\n".into()),
        Err(error) => Err(error),
    }
}

/// Structured views, raw records, literal values and already rendered
/// presentations have different boundaries. Never infer these from content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Output {
    Json,
    Records,
    Literal,
    Rendered,
}
fn output_contract(args: &[String]) -> Output {
    let words: Vec<_> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["state", "--schema" | "--json", ..] => Output::Json,
        ["log", _, rest @ ..] if rest.contains(&"--raw") => Output::Records,
        ["status" | "list" | "lifecycle", ..] | ["log", ..] => Output::Rendered,
        _ => Output::Literal,
    }
}

fn scrub_output(text: &str, output: Output) -> Result<String, String> {
    match output {
        // Their owners redact independent fields before joining them, and
        // redact complete payloads before summary/tail truncation.
        Output::Rendered => Ok(text.to_owned()),
        Output::Literal => Ok(burrow_probe::scrub::Redactor::default().plain(text)),
        Output::Records => burrow_probe::scrub::Jsonl::default().text(text),
        Output::Json => {
            let value = serde_json::from_str(text).map_err(|e| e.to_string())?;
            let value = burrow_probe::scrub::independent_json(&value)?;
            let mut clean = serde_json::to_string(&value).map_err(|e| e.to_string())?;
            if text.ends_with('\n') {
                clean.push('\n');
            }
            Ok(clean)
        }
    }
}

pub(crate) fn recovery_reply(cfg: &Config, args: &[String]) -> Result<String, Fail> {
    let mut words = vec!["reply".into()];
    words.extend_from_slice(args);
    validate(&words).map_err(|e| Fail::new(2, e))?;
    let (mut i, mut first, mut cutoff) = (0, false, None);
    while args[i].starts_with("--") {
        match args[i].as_str() {
            "--first" => first = true,
            "--cut-off" => {
                cutoff = Some(args[i + 1].as_str());
                i += 1;
            }
            _ => unreachable!("validated recovery flags"),
        }
        i += 1;
    }
    crate::cmd::reply(cfg, &args[i], &args[i + 1], first, cutoff)?;
    Ok(format!("{}\n", args[i]))
}

#[cfg(test)]
mod tests {
    use super::{Output, output_contract, scrub_output};

    #[test]
    fn formatted_json_jsonl_and_mixed_diagnostics_keep_their_structure() {
        let record = serde_json::json!({"message":"Authorization: credential\npublic \"quotes\"",
            "password":"escaped \"credential\"\nsecond line", "count":3});
        let pretty = format!("{}\n", serde_json::to_string_pretty(&record).unwrap());
        let output = scrub_output(&pretty, Output::Records).unwrap();
        let decoded: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(decoded["count"], 3);
        assert_eq!(decoded["password"], "[redacted]");
        assert_eq!(
            decoded["message"],
            "Authorization: [redacted]\npublic \"quotes\""
        );
        assert!(!output.contains("credential"));
        let lines = format!("{record}\r\n{record}\n");
        let clean = scrub_output(&lines, Output::Records).unwrap();
        assert!(clean.contains("\r\n"));
        assert_eq!(
            clean
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>(),
            vec![decoded.clone(), decoded]
        );
        let key = format!(
            "-----BEGIN RSA {} KEY-----\ncredential payload\n-----END RSA {} KEY-----\n",
            "PRIVATE", "PRIVATE"
        );
        let mixed = scrub_output(
            &format!("diagnostic\n{key}{record}\npartial password=\"credential"),
            Output::Records,
        )
        .unwrap();
        assert!(!mixed.contains("credential"));
        assert!(mixed.contains("[redacted private key]"));
        assert!(mixed.contains("\"count\":3"));
    }

    #[test]
    fn literal_names_keep_every_byte_and_only_structured_commands_parse_json() {
        let name = "my\n 42 \napp\n{\"a\": 1}\n";
        assert_eq!(scrub_output(name, Output::Literal).unwrap(), name);
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(output_contract(&words(&["state", "--json"])), Output::Json);
        assert_eq!(
            output_contract(&words(&["log", "j", "--raw"])),
            Output::Records
        );
        for plain in [
            &["find", "k"][..],
            &["list"],
            &["reply", "j", "x"],
            &["log", "j"],
        ] {
            assert!(!matches!(
                output_contract(&words(plain)),
                Output::Json | Output::Records
            ));
        }
    }

    #[test]
    fn interleaved_metadata_and_other_calls_cannot_close_an_outputs_secret() {
        let records = [
            serde_json::json!({"call_id":"a", "output":"password=\"first"}),
            serde_json::json!({"call_id":"b", "output":"public output \"quoted\""}),
            serde_json::json!({"call_id":"a", "command":"cat \"$file\""}),
            serde_json::json!({"call_id":"a", "output":"second\" public suffix"}),
        ];
        let text = records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>();
        let out = scrub_output(&text, Output::Records).unwrap();
        let values: Vec<serde_json::Value> = out
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(values[1]["output"], records[1]["output"]);
        assert_eq!(values[2]["command"], records[2]["command"]);
        assert!(!out.contains("first") && !out.contains("second"), "{out}");
        assert!(
            values[3]["output"]
                .as_str()
                .unwrap()
                .contains("public suffix")
        );
    }
}
