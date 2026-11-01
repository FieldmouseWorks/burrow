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
    let rules: &[(&str, &str)] = &[
        // A private key, PEM or PGP, to its END line, or to the end of the
        // text when that was cut off. A journal puts each of its lines on
        // a line of its own, timestamp first; the block goes whole.
        (
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
            "[redacted private key]",
        ),
        // Headers that carry a credential, to the end of the line.
        (
            r"(?im)\b((?:proxy-)?authorization|x-api-key|api-key|x-auth-token|private-token|cookie|set-cookie)(\s*[:=]\s*)[^\r\n]*",
            "$1$2[redacted]",
        ),
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
        // a secret.
        (
            r#"(?i)\b([a-z0-9_.-]*(?:password|passwd|passphrase|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret|credentials?))(["']?\s*[:=]\s*["']?)([^\s"',;&]+)"#,
            "$1$2[redacted]",
        ),
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

/// `text` with every secret it can tell for one replaced.
pub fn scrub(text: &str) -> Scrubbed {
    let mut out = text.to_string();
    for (find, with) in RULES.iter() {
        if find.is_match(&out) {
            out = find.replace_all(&out, with.as_str()).into_owned();
        }
    }
    Scrubbed(out)
}

#[cfg(test)]
mod tests {
    use super::scrub;

    // Each secret is put together here, so no tracked file holds one
    // (burrow's own secrets check reads them).
    fn github() -> String {
        format!("gh{}_{}", "p", "a1B2c3D4e5".repeat(4))
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
}
