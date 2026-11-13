//! What leaves the machine. Every answer goes through [`scrub`] on its way to
//! GitHub, and a comment is posted only from a [`Scrubbed`], which nothing
//! else makes (pass.rs). The journal allowlist is the first guard (a unit
//! whose log holds secrets isn't on it); this is the second, for a secret
//! that turns up where nobody expected one. It knows secrets by their shape:
//! the formats tokens and keys come in, and the places a log puts them
//! (headers, URLs, `password=` and its kind). A secret with no shape, a bare
//! random string on a line of its own, gets through it, which is why the
//! allowlist comes first. It errs on the side of hiding: a value that only
//! looks like a secret is hidden too.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

mod streams;
pub use streams::Jsonl;

const HEADER_NAME: &str =
    "(?:proxy-)?authorization|x-api-key|api-key|x-auth-token|private-token|cookie|set-cookie";
const SECRET_NAME: &str = "[a-z0-9_.-]*(?:password|passwd|passphrase|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret|credentials?)";
static SECRET_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)\b(?:{HEADER_NAME}|{SECRET_NAME})$")).expect("credential field names")
});

/// What a secret is replaced with.
pub const REDACTED: &str = "[redacted]";

/// Text that has been through [`scrub`]: the only thing pass.rs posts.
#[derive(Debug)]
pub struct Scrubbed(String);

impl Scrubbed {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Each rule: what it finds, and what goes in its place (`$1` and the like
/// keep the parts around the secret). In order: a rule that hides a whole
/// line goes before the ones that would hide a part of it.
static RULES: LazyLock<Vec<(Regex, String)>> = LazyLock::new(|| {
    let header = format!(r"(?im)\b({HEADER_NAME})(\s*[:=]\s*)[^\r\n]*");
    let double =
        format!(r#"(?i)\b({SECRET_NAME})(["']?\s*[:=]\s*)"(?:[^"\\\r\n]|\\[^\r\n])*\\?"?"#);
    let single =
        format!(r#"(?i)\b({SECRET_NAME})(["']?\s*[:=]\s*)'(?:[^'\\\r\n]|\\[^\r\n])*\\?'?"#);
    let bare = format!(r#"(?i)\b({SECRET_NAME})(["']?\s*[:=]\s*["']?)([^\s"',;&]+)"#);
    let rules: &[(&str, &str)] = &[
        // A private key, PEM or PGP, to its END line, or to the end of the
        // text when that was cut off. A journal puts each of its lines on
        // a line of its own, timestamp first; the block goes whole.
        (
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
            "[redacted private key]",
        ),
        // A key's END with no BEGIN before it, which a journal's line limit
        // can leave. Every whole block is gone by now, so each END left is
        // one of those, and everything from the start of the text to the
        // last of them goes, however many fragments there are.
        (
            r"(?s)\A.*-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----",
            "[redacted private key]",
        ),
        // Headers that carry a credential, to the end of the line.
        (&header, "$1$2[redacted]"),
        (
            r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}",
            "$1 [redacted]",
        ),
        // "token" is a word in plain prose too: only a long run after it.
        (r"(?i)\b(token)\s+[A-Za-z0-9._~+/=-]{20,}", "$1 [redacted]"),
        // A URL's user and password (or a token in their place).
        (r"(?i)\b([a-z][a-z0-9+.-]*://)[^\s/@]+@", "$1[redacted]@"),
        // A Healthchecks ping URL's key and check: whoever has them can
        // report for the machine.
        (r"(?i)\b(hc-ping\.com/)[^\s]+", "$1[redacted]"),
        // Tokens by their format.
        (r"\bgh[pousr]_[A-Za-z0-9]{20,}", "[redacted]"),
        (r"\bgithub_pat_[A-Za-z0-9_]{20,}", "[redacted]"),
        (r"\bv1\.[0-9a-f]{40}\b", "[redacted]"),
        (r"\bglpat-[A-Za-z0-9_-]{20,}", "[redacted]"),
        (r"\bsk-[A-Za-z0-9_-]{20,}", "[redacted]"),
        (r"\b[rs]k_(?:live|test)_[0-9A-Za-z]{16,}", "[redacted]"),
        (r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b", "[redacted]"),
        (r"\bAIza[0-9A-Za-z_-]{35}", "[redacted]"),
        (r"\bxox[abposr]-[A-Za-z0-9-]{10,}", "[redacted]"),
        (r"\btskey-[A-Za-z0-9-]{10,}", "[redacted]"),
        (r"\bAGE-SECRET-KEY-1[0-9A-Z]{20,}", "[redacted]"),
        (r"\bnpm_[A-Za-z0-9]{36}", "[redacted]"),
        (
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
            "[redacted]",
        ),
        // NAME=VALUE, NAME: VALUE, "NAME": "VALUE", where the name says it's
        // a secret. A quoted value goes whole, whatever it holds: past
        // escaped quotes, a dangling backslash too, to its closing quote or,
        // with none, the line's end (LF, CRLF or the text's). Nothing stops
        // it short, so the unquoted rule after never gets half of one; that
        // one is for a bare value, to the first character that ends one.
        (&double, "$1$2\"[redacted]\""),
        (&single, "$1$2'[redacted]'"),
        (&bare, "$1$2[redacted]"),
    ];
    rules
        .iter()
        .map(|(find, with)| {
            (
                Regex::new(find).expect("a scrub rule that compiles"),
                (*with).to_string(),
            )
        })
        .collect()
});

/// How many of [`RULES`]' first entries are the private-key ones.
const KEY_RULES: usize = 2;

/// `text` with every secret it can tell for one replaced.
pub fn scrub(text: &str) -> Scrubbed {
    Scrubbed(apply(&RULES, text))
}

/// `text` with only its private keys taken out, each block whole: what a
/// request goes through before its lines are read as questions. A key is
/// the one secret only its block's other lines identify, so this has to
/// see them all at once; the other rules would rewrite a valid question
/// (a commit hash after `token`, a unit named `secret:x`), so they're left
/// to what gets posted. Not a [`Scrubbed`]: nothing posts it as it is.
pub fn redact_keys(text: &str) -> String {
    apply(&RULES[..KEY_RULES], text)
}

fn apply(rules: &[(Regex, String)], text: &str) -> String {
    let mut out = text.to_string();
    for (find, with) in rules {
        if find.is_match(&out) {
            out = find.replace_all(&out, with.as_str()).into_owned();
        }
    }
    out
}

static KEY_BEGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----").expect("key begin")
});
static KEY_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----").expect("key end")
});
// An opening delimiter, not a replacement regex: closing delimiters and
// private-key markers must be observed in the original, unmodified text.
static CREDENTIAL_OPEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)\b({SECRET_NAME}|{HEADER_NAME})(["']?\s*[:=]\s*)(["'])"#
    ))
    .expect("credential opening quote")
});

/// How much raw text is kept to see a delimiter split across two pieces.
const TAIL: usize = 64;

/// Redacts a sequence of texts (decoded JSON strings, records, plain
/// segments of one stream) while remembering what is still open at the end
/// of each: a private key, a delimiter cut in two, a quoted credential. The
/// next piece finishes them, so a secret split across values, records or
/// segments goes whole.
#[derive(Default)]
pub struct Redactor {
    in_key: bool,
    /// An unclosed credential quote: its character, and whether the piece
    /// ended on the backslash that escapes the next character.
    quote: Option<(char, bool)>,
    /// The last raw characters seen, for a delimiter cut between pieces.
    tail: String,
}

enum Mask {
    Key,
    Quoted(String),
    Continuation,
}

/// Where the quote `q` closes in `text`, past `after` (the byte it starts
/// at); `Err` says whether the text ended on a pending escape.
fn close_of(text: &str, after: usize, q: char, mut escaped: bool) -> Result<usize, bool> {
    for (i, c) in text[after..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == q {
            return Ok(after + i + c.len_utf8());
        }
    }
    Err(escaped)
}

impl Redactor {
    /// Plain text, whose lines are lines.
    pub fn plain(&mut self, text: &str) -> String {
        self.run(text)
    }

    fn remember(&mut self, text: &str) {
        self.tail.push_str(text);
        let keep = self
            .tail
            .char_indices()
            .rev()
            .nth(TAIL - 1)
            .map_or(0, |(i, _)| i);
        self.tail.drain(..keep);
    }

    fn run(&mut self, text: &str) -> String {
        let seen = self.tail.len();
        let joined = format!("{}{text}", self.tail);
        let mut spans = Vec::new();

        // Key and quote automata inspect the same raw bytes independently.
        // Their hidden spans are united only after both states are updated.
        let mut markers = KEY_BEGIN
            .find_iter(&joined)
            .filter(|m| m.end() > seen)
            .map(|m| (m.start().saturating_sub(seen), m.end() - seen, true))
            .chain(
                KEY_END
                    .find_iter(&joined)
                    .filter(|m| m.end() > seen)
                    .map(|m| (m.start().saturating_sub(seen), m.end() - seen, false)),
            )
            .collect::<Vec<_>>();
        markers.sort_unstable();
        let mut start = self.in_key.then_some(0);
        for (at, end, begin) in markers {
            if begin {
                start.get_or_insert(at);
            } else {
                spans.push((start.take().unwrap_or(0), end, Mask::Key));
            }
        }
        self.in_key = start.is_some();
        if let Some(start) = start {
            spans.push((start, text.len(), Mask::Key));
        }

        let mut cursor = 0;
        if let Some((quote, escaped)) = self.quote.take() {
            match close_of(text, 0, quote, escaped) {
                Ok(end) => {
                    spans.push((0, end, Mask::Continuation));
                    cursor = end;
                }
                Err(escaped) => {
                    self.quote = Some((quote, escaped));
                    spans.push((0, text.len(), Mask::Continuation));
                    cursor = text.len();
                }
            }
        }
        for caps in CREDENTIAL_OPEN.captures_iter(&joined) {
            let whole = caps.get(0).unwrap();
            if whole.end() <= seen {
                continue;
            }
            let quote_at = caps.get(3).unwrap();
            let at = quote_at.start().saturating_sub(seen);
            if at < cursor {
                continue;
            }
            let quote = quote_at.as_str().chars().next().unwrap();
            let after = whole.end() - seen;
            let (end, escaped) = match close_of(text, after, quote, false) {
                Ok(end) => (end, None),
                Err(escaped) => (text.len(), Some(escaped)),
            };
            // Keep the field label and spacing without running text rules
            // over them again (a header rule would eat the public suffix).
            let begin = whole.start().saturating_sub(seen);
            let prefix = &joined[whole.start().max(seen)..quote_at.start()];
            spans.push((
                begin,
                end,
                Mask::Quoted(format!("{prefix}{quote}{REDACTED}{quote}")),
            ));
            cursor = end;
            if let Some(escaped) = escaped {
                self.quote = Some((quote, escaped));
                break;
            }
        }
        self.remember(text);
        spans.sort_by_key(|(start, end, _)| (*start, *end));
        let mut merged: Vec<(usize, usize, Mask)> = Vec::new();
        for (start, end, mask) in spans {
            if let Some(last) = merged.last_mut().filter(|last| start < last.1) {
                last.1 = last.1.max(end);
                if matches!(mask, Mask::Key) {
                    last.2 = Mask::Key;
                }
            } else {
                merged.push((start, end, mask));
            }
        }
        let mut out = String::new();
        let mut from = 0;
        for (start, end, mask) in merged {
            out.push_str(scrub(&text[from..start]).as_str());
            match mask {
                Mask::Key => out.push_str("[redacted private key]"),
                Mask::Quoted(replacement) => out.push_str(&replacement),
                Mask::Continuation => {}
            }
            from = end;
        }
        out.push_str(scrub(&text[from..]).as_str());
        out
    }

    /// Redact decoded JSON values before serialization so text rules cannot
    /// consume its delimiters or escape sequences. Credential fields are
    /// hidden whole, including multiline strings and non-string credentials.
    pub fn json(&mut self, value: &Value) -> Result<Value, String> {
        Ok(match value {
            Value::String(text) => Value::String(self.run(text)),
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|v| self.json(v))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(fields) => {
                let mut clean = serde_json::Map::new();
                for (key, value) in fields {
                    let value = if SECRET_FIELD.is_match(key) {
                        Value::String(REDACTED.into())
                    } else {
                        // Object fields describe independent values, not
                        // consecutive pieces of their neighbours' text.
                        Redactor::default().json(value)?
                    };
                    insert(&mut clean, key, value)?;
                }
                Value::Object(clean)
            }
            value => value.clone(),
        })
    }
}

fn insert(
    fields: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Value,
) -> Result<(), String> {
    if fields.insert(scrub(key).0, value).is_some() {
        return Err("JSON property names collide after redaction".into());
    }
    Ok(())
}

/// One decoded value, redacted on its own.
pub fn json(value: &Value) -> Result<Value, String> {
    Redactor::default().json(value)
}

/// A structured view (for example a snapshot) contains independent values,
/// including entries in arrays of names. It has no continuation semantics.
pub fn independent_json(value: &Value) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => Value::String(Redactor::default().plain(text)),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(independent_json)
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(fields) => {
            let mut clean = serde_json::Map::new();
            for (key, value) in fields {
                let value = if SECRET_FIELD.is_match(key) {
                    Value::String(REDACTED.into())
                } else {
                    independent_json(value)?
                };
                insert(&mut clean, key, value)?;
            }
            Value::Object(clean)
        }
        value => value.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::scrub;

    // Each secret is put together here, so no tracked file holds one
    // (burrow's own secrets check reads them).
    fn github() -> String {
        format!("gh{}_{}", "p", "a1B2c3D4e5".repeat(4))
    }

    #[test]
    fn structured_values_hide_credentials_without_touching_json_delimiters() {
        let source = serde_json::json!({
            "Authorization":"header credential",
            "client_secret":"quoted \"credential\"\nand a second line",
            "array":[{"api-key":{"nested":"credential"}, "count":9}],
            "message":"Authorization: header credential\npublic line \"with quotes\"",
            "input_tokens":42, "public":true
        });
        let value = super::json(&source).unwrap();
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains("credential"));
        assert_eq!(value["Authorization"], "[redacted]");
        assert_eq!(value["client_secret"], "[redacted]");
        assert_eq!(value["array"][0]["api-key"], "[redacted]");
        assert_eq!(value["array"][0]["count"], 9);
        assert_eq!(value["input_tokens"], 42);
        assert_eq!(value["public"], true);
        assert_eq!(
            value["message"],
            "Authorization: [redacted]\npublic line \"with quotes\""
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
            value
        );
    }

    #[test]
    fn multiline_credentials_go_whole_in_decoded_strings_and_across_values() {
        let begin = format!("-----BEGIN RSA {} KEY-----", "PRIVATE");
        let end = format!("-----END RSA {} KEY-----", "PRIVATE");
        let value = super::json(&serde_json::json!({
            "a": "password=\"first\nsecond\" public",
            "b": [format!("x {begin}"), "MIIEbase64payload", format!("tail {end} after")],
        }))
        .unwrap();
        let text = value.to_string();
        assert!(
            !text.contains("second") && !text.contains("base64payload"),
            "{text}"
        );
        assert!(text.contains("public") && text.contains("after"), "{text}");
        let mut r = super::Redactor::default();
        r.plain(&format!("{begin}\nMIIE"));
        assert!(!r.plain("morepayload\n").contains("payload"));
        assert!(!r.plain(&format!("last\n{end}\nok")).contains("last"));
        assert!(r.plain("ok").contains("ok"));
    }

    #[test]
    fn redacting_property_names_never_silently_overwrites_another_field() {
        let first = github();
        let second = format!("gh{}_{}", "p", "different".repeat(4));
        let source = serde_json::json!({first:1, second:2});
        assert!(super::json(&source).unwrap_err().contains("collide"));
    }

    fn hidden(line: &str, secret: &str) {
        let out = scrub(line);
        assert!(
            !out.as_str().contains(secret),
            "{secret:?} got through: {:?}",
            out.as_str()
        );
        assert!(out.as_str().contains("[redacted"), "{:?}", out.as_str());
    }

    #[test]
    fn tokens_by_their_format() {
        let gh = github();
        hidden(&format!("cloned with {gh} ok"), &gh);
        for t in [
            format!(
                "github_{}_{}",
                "pat", "11ABCDEFG0123456789_abcdefghijklmnop"
            ),
            format!("s{}-ant-api03-{}", "k", "x".repeat(40)),
            format!("s{}-proj-{}", "k", "Y7".repeat(15)),
            format!("AK{}{}", "IA", "ABCDEFGHIJ234567"),
            format!("xo{}-{}", "xb", "1234567890-abcdefghij"),
            format!("tsk{}-auth-{}", "ey", "kAbCdEfGh1CNTRL-0123456789"),
            format!(
                "AGE-{}-KEY-1{}",
                "SECRET", "QWERTYUIOPASDFGHJKLZXCVBNM0123456789QWERTYUIOPASDFGHJ"
            ),
            format!(
                "ey{}.{}.{}",
                "JhbGciOiJIUzI1NiJ9", "eyJzdWIiOiIxMjM0In0", "abcdefghijklmnop"
            ),
        ] {
            hidden(
                &format!("2026-11-01T10:00:00Z remi unit[1]: got {t} back"),
                &t,
            );
        }
    }

    #[test]
    fn where_logs_put_them() {
        let gh = github();
        hidden(&format!("> Authorization: token {gh}"), &gh);
        hidden(
            "Authorization: Basic dXNlcjpwYXNzd29yZA==",
            "dXNlcjpwYXNzd29yZA==",
        );
        hidden(&format!("curl -H 'Authorization: Bearer {gh}'"), &gh);
        hidden(
            "Bearer abcdefghijklmnopqrstuvwxyz012345",
            "abcdefghijklmnopqrstuvwxyz012345",
        );
        hidden(
            &format!("fetching https://x-access-token:{gh}@github.com/o/r.git"),
            &gh,
        );
        hidden("https://ci:hunter2hunter2@example.com/", "hunter2hunter2");
        hidden(
            "ping https://hc-ping.com/abcdEFGH1234/remi failed",
            "abcdEFGH1234",
        );
        hidden("db password=correct-horse-battery", "correct-horse-battery");
        hidden(r#"{"client_secret": "s3cr3t-value"}"#, "s3cr3t-value");
        hidden("GH_TOKEN=whatever-it-is", "whatever-it-is");
        hidden("api_key: 0123456789abcdef", "0123456789abcdef");
    }

    #[test]
    fn a_quoted_value_goes_whole() {
        for (line, secret) in [
            (
                r#"password="correct horse battery staple""#,
                "horse battery staple",
            ),
            (r#"password=";entire-value""#, "entire-value"),
            (r#"password="a, b; c & d""#, "b; c & d"),
            (
                r#"{"client_secret": "with \"escaped\" quotes, and more"}"#,
                "quotes, and more",
            ),
            (r#"{"api_key": "a\\\"b cAd"}"#, "b c"),
            ("secret='single quoted; value here'", "value here"),
            ("token = 'it\\'s still secret'", "still secret"),
            (r#"password="cut off at the line's end"#, "at the line"),
            // A dangling backslash, at the text's end or a line's, LF or CRLF.
            ("password=\"correct horse\\", "horse"),
            ("password=\"correct horse\\\nnext", "horse"),
            ("password=\"correct horse\r\nnext", "horse"),
            ("password=\"correct horse\\\r\nnext", "horse"),
            ("secret='single quoted \\", "quoted"),
            ("secret='single quoted\r\nnext", "quoted"),
        ] {
            hidden(line, secret);
            // However many times it's scrubbed.
            let twice = scrub(scrub(line).as_str());
            assert!(!twice.as_str().contains(secret), "{:?}", twice.as_str());
        }
        // The line after an unclosed one stays.
        assert_eq!(
            scrub("password=\"a b\r\nnext line").as_str(),
            "password=\"[redacted]\"\r\nnext line"
        );
        assert_eq!(
            scrub(r#"db password="a b" user="sam""#).as_str(),
            r#"db password="[redacted]" user="sam""#
        );
    }

    #[test]
    fn a_private_key_goes_whole() {
        let key = format!(
            "-----BEGIN OPENSSH {} KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nQUJDREVGR0hJSktMTU5PUFFS\n-----END OPENSSH {} KEY-----",
            "PRIVATE", "PRIVATE"
        );
        let out = scrub(&format!("before\n{key}\nafter"));
        assert_eq!(out.as_str(), "before\n[redacted private key]\nafter");
        // Cut off before its end: everything from its start goes.
        let cut = format!(
            "before\n-----BEGIN RSA {} KEY-----\nMIIEow\nIBAAKC",
            "PRIVATE"
        );
        assert_eq!(scrub(&cut).as_str(), "before\n[redacted private key]");
        // Several with their BEGINs cut off, and a whole one among them:
        // every payload goes, to the last END.
        let (end, begin) = (
            format!("-----END RSA {} KEY-----", "PRIVATE"),
            format!("-----BEGIN RSA {} KEY-----", "PRIVATE"),
        );
        let many = format!(
            "payloadone\n{end}\npayloadtwo\n{end}\n{begin}\npayloadwhole\n{end}\npayloadthree\n{end}\nafter"
        );
        let out = scrub(&many);
        assert_eq!(out.as_str(), "[redacted private key]\nafter");
        assert_eq!(
            scrub(out.as_str()).as_str(),
            "[redacted private key]\nafter"
        );
        // Its BEGIN cut off: everything up to its END goes.
        let tail = format!(
            "QUJDREVGR0hJSktMTU5PUFFS\n-----END RSA {} KEY-----\nafter",
            "PRIVATE"
        );
        assert_eq!(scrub(&tail).as_str(), "[redacted private key]\nafter");
    }

    #[test]
    fn what_isnt_a_secret_stays() {
        for line in [
            "Started remi-update.service - remi: update to main.",
            "nix-daemon[812]: building '/nix/store/4b2a1c0d8q9wxyz3abcdfghijklmnpqr-burrow-probe-0.0.0.drv'",
            "deployed 3520fa4c1b9e2d7f6a8b0c3d5e7f9a1b2c4d6e8f on main",
            "input_tokens=1234 output_tokens=56 cache_read_input_tokens=7",
            "proj-12345.service: Consumed 3min 2.5s CPU time, 1.2G memory peak.",
            "Out of memory: Killed process 4242 (nix) total-vm:9000000kB",
        ] {
            assert_eq!(scrub(line).as_str(), line);
        }
    }

    #[test]
    fn plain_text_and_fragments_hide_multiline_and_split_secrets() {
        let mut r = super::Redactor::default();
        let out = r.plain("password=\"first\nsecond\" public");
        assert!(!out.contains("second") && out.contains("public"));
        // A BEGIN delimiter cut in two, then a payload.
        let mut r = super::Redactor::default();
        let v = r.json(&serde_json::json!([
            "x -----BEGIN RSA PRIVATE ",
            "KEY-----\nPAYLOAD",
            "MORE",
            "-----END RSA PRIVATE ",
            "KEY----- tail"
        ]));
        let out = v.unwrap().to_string();
        assert!(!out.contains("PAYLOAD") && !out.contains("MORE"), "{out}");
        assert!(out.contains("tail"), "{out}");
        // A quoted credential split across values.
        let mut r = super::Redactor::default();
        let out = r
            .json(&serde_json::json!([
                "token=\"abc",
                "def\\\"ghi",
                "jkl\" after"
            ]))
            .unwrap()
            .to_string();
        for leak in ["abc", "def", "ghi", "jkl"] {
            assert!(!out.contains(leak), "{out}");
        }
        assert!(out.contains("after"), "{out}");
    }

    #[test]
    fn independent_fields_do_not_inherit_another_values_open_secret() {
        let source = serde_json::json!({"output":"password=\"first", "command":"cat \"$file\"", "state":"done", "id":"public-job"});
        let value = super::json(&source).unwrap();
        assert_eq!(value["command"], source["command"]);
        assert_eq!(value["id"], "public-job");
        assert_eq!(value["state"], "done");
        assert!(!value["output"].as_str().unwrap().contains("first"));
        let names = serde_json::json!(["password=\"first", "cat \"$file\"", "next public"]);
        let clean = super::independent_json(&names).unwrap();
        assert_eq!(clean[1], names[1]);
        assert_eq!(clean[2], names[2]);
    }

    #[test]
    fn overlapping_quoted_keys_close_both_states_and_preserve_public_suffixes() {
        let begin = format!("-----BEGIN RSA {} ", "PRIVATE");
        let end = format!("-----END RSA {} KEY-----", "PRIVATE");
        for pieces in [
            vec![
                format!("password=\"{begin}"),
                format!("KEY-----\nPAYLOAD\n{end}\" public suffix"),
                "following public record".into(),
            ],
            vec![
                format!("password=\"{begin}"),
                "KEY-----\nPAY\\\"LOAD\n-----END RSA PRIVATE ".into(),
                "KEY-----\" public suffix".into(),
                "following public record".into(),
            ],
        ] {
            let mut redactor = super::Redactor::default();
            let output = pieces
                .iter()
                .map(|piece| redactor.plain(piece))
                .collect::<String>();
            assert!(
                !output.contains("PAYLOAD") && !output.contains("LOAD"),
                "{output}"
            );
            assert!(
                output.contains("public suffix") && output.contains("following public record"),
                "{output}"
            );
        }
    }

    #[test]
    fn a_quote_opened_inside_a_key_keeps_hiding_after_the_key_ends() {
        let mut redactor = super::Redactor::default();
        let pieces = [
            format!("-----BEGIN RSA {} KEY-----", "PRIVATE"),
            "password=\"FIRSTSECRET".into(),
            format!("-----END RSA {} KEY----- SECONDSECRET", "PRIVATE"),
            "\" public suffix".into(),
            "following public record".into(),
        ];
        let out = pieces
            .iter()
            .map(|text| redactor.plain(text))
            .collect::<String>();
        assert!(
            !out.contains("FIRSTSECRET") && !out.contains("SECONDSECRET"),
            "{out}"
        );
        assert!(
            out.contains("public suffix") && out.contains("following public record"),
            "{out}"
        );
    }
}
