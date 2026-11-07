//! A Responses API stand-in for the pinned Codex CLI, for the VM test
//! tests/proj-codex-sandbox.nix: `codex-stand-in PORT CALLS`. Each turn
//! replays the tool calls in the JSON file CALLS (`[[NAME, ARGS], ...]`,
//! read afresh for each request), one per request, then answers. A
//! `"SID"` session_id stands for the session the last call left running.
//! apply_patch is Codex's freeform tool: its ARGS' input is the patch.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, port, calls] = args.as_slice() else {
        eprintln!("usage: codex-stand-in PORT CALLS");
        std::process::exit(2);
    };
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap_or_else(|e| {
        eprintln!("codex-stand-in: 127.0.0.1:{port}: {e}");
        std::process::exit(1);
    });
    for stream in listener.incoming().flatten() {
        let calls = calls.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve(stream, &calls) {
                eprintln!("codex-stand-in: {e}");
            }
        });
    }
}

/// One HTTP/1.0 exchange: a POST's body in, its events out as
/// server-sent events; anything else isn't implemented.
fn serve(stream: TcpStream, calls: &str) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let post = line.starts_with("POST ");
    let mut length = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut out = stream;
    if !post {
        return out.write_all(b"HTTP/1.0 501 Not Implemented\r\ncontent-length: 0\r\n\r\n");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let request: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let calls: Vec<(String, Value)> = serde_json::from_str(&std::fs::read_to_string(calls)?)?;
    let mut text = String::from("HTTP/1.0 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
    for event in events(&request, &calls) {
        let kind = event["type"].as_str().unwrap_or_default();
        text.push_str(&format!("event: {kind}\ndata: {event}\n\n"));
    }
    out.write_all(text.as_bytes())
}

/// The events answering `request`: the next of `calls` while there's one
/// its tool outputs haven't answered, then a message.
fn events(request: &Value, calls: &[(String, Value)]) -> Vec<Value> {
    let outputs: Vec<String> = request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["type"].as_str().is_some_and(|t| t.ends_with("_output")))
        .map(|i| match &i["output"] {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .collect();
    let item = match calls.get(outputs.len()) {
        Some((name, args)) => {
            let mut args = args.clone();
            if args["session_id"] == "SID"
                && let Some(session) = session(&outputs.join(" "))
            {
                args["session_id"] = json!(session);
            }
            let call_id = format!("c{}", outputs.len());
            if name == "apply_patch" {
                json!({"type": "custom_tool_call", "name": name, "input": args["input"], "call_id": call_id})
            } else {
                json!({"type": "function_call", "name": name, "arguments": args.to_string(), "call_id": call_id})
            }
        }
        None => {
            json!({"type": "message", "role": "assistant", "id": "m", "content": [{"type": "output_text", "text": "done"}]})
        }
    };
    let usage = json!({"input_tokens": 1, "input_tokens_details": null, "output_tokens": 1, "output_tokens_details": null, "total_tokens": 2});
    vec![
        json!({"type": "response.created", "response": {"id": "r"}}),
        json!({"type": "response.output_item.done", "item": item}),
        json!({"type": "response.completed", "response": {"id": "r", "usage": usage}}),
    ]
}

/// The first session a tool output names ("session ID 12").
fn session(text: &str) -> Option<u64> {
    text.match_indices("session ID ").find_map(|(at, said)| {
        let rest = &text[at + said.len()..];
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..digits].parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(request: Value, calls: &[(String, Value)]) -> Value {
        events(&request, calls)[1]["item"].clone()
    }

    #[test]
    fn each_call_in_turn_then_an_answer() {
        let calls = vec![
            ("exec_command".to_owned(), json!({"cmd": "sleep 60"})),
            (
                "write_stdin".to_owned(),
                json!({"session_id": "SID", "chars": "rm x\n"}),
            ),
            (
                "apply_patch".to_owned(),
                json!({"input": "*** Begin Patch\n*** End Patch\n"}),
            ),
        ];
        let first = item(json!({"input": [{"type": "message"}]}), &calls);
        assert_eq!(first["type"], "function_call");
        assert_eq!(first["call_id"], "c0");
        let args: Value = serde_json::from_str(first["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args, json!({"cmd": "sleep 60"}));
        let ran =
            json!({"type": "function_call_output", "output": "Process running with session ID 42"});
        let second = item(json!({"input": [ran]}), &calls);
        let args: Value = serde_json::from_str(second["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["session_id"], 42);
        let typed = json!({"type": "function_call_output", "output": {"text": "ok"}});
        let third = item(json!({"input": [ran, typed]}), &calls);
        assert_eq!(third["type"], "custom_tool_call");
        assert_eq!(third["input"], "*** Begin Patch\n*** End Patch\n");
        let patched = json!({"type": "custom_tool_call_output", "output": "Done"});
        let last = item(json!({"input": [ran, typed, patched]}), &calls);
        assert_eq!(last["type"], "message");
        assert_eq!(last["content"][0]["text"], "done");
    }

    #[test]
    fn a_session_is_the_first_one_named() {
        assert_eq!(session("x session ID 7 y session ID 8"), Some(7));
        assert_eq!(session("session ID x"), None);
        assert_eq!(session(""), None);
    }

    /// The real exchange: a POST over TCP, answered as server-sent events.
    #[test]
    fn a_post_is_answered_with_events() {
        let dir = std::env::temp_dir().join(format!("codex-stand-in-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let calls = dir.join("calls");
        std::fs::write(&calls, "[]").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let path = calls.to_str().unwrap().to_owned();
        std::thread::spawn(move || serve(listener.accept().unwrap().0, &path).unwrap());
        let mut client = TcpStream::connect(at).unwrap();
        let body = r#"{"input":[]}"#;
        write!(
            client,
            "POST /v1/responses HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.0 200 OK\r\n"), "{answer}");
        assert!(
            answer.contains("event: response.output_item.done\ndata: "),
            "{answer}"
        );
        assert!(answer.contains(r#""text":"done""#), "{answer}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
