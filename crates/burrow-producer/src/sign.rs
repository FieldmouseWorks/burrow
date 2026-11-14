//! The signer (Conary's `docs/m6-plugins.md`, "One key per producer" and
//! "No signing verb"). A producer's Ed25519 key comes only from its
//! systemd credential, which only its own user can read: never from the
//! environment, an argument or a path. It signs only the producer's own
//! results, each from the record of that result ([`Review`], [`Judgment`],
//! [`Proof`]), as Conary's service statement (`attest::service`), with
//! Conary's receipts as the log. Nothing here takes a statement to sign.
use conary_core::plugin::{Judgment as Verdict, Provenance};
use conary_github::attest::service::{self, Log, Revision, Statement};
use conary_github::flow::{self, Recipe};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::Path;

/// The check a producer's review, judgment or proof is published as.
const JUDGMENT: &str = "conary/judgment";

/// A review the flow published, as the review producer recorded the look
/// that found it: signed as `conary/review/ROLE` over
/// `review_receipt(head, body)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub repo: String,
    pub role: String,
    pub head: String,
    /// The head's tree.
    pub tree: String,
    /// The published review's body, exactly.
    pub body: String,
}

/// A judgment the judge producer issued: signed as `conary/judgment` over
/// `judgment_receipt_by(revision, generation, verdict, provenance)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Judgment {
    pub repo: String,
    pub revision: flow::Revision,
    /// The head's tree.
    pub tree: String,
    pub generation: u64,
    pub verdict: Option<Verdict>,
    pub provenance: Option<Provenance>,
}

/// A proof's run, as the proof producer recorded its program's exit:
/// signed as the proof's check, defined by its recipe's digest, over the
/// SHA-256 of the program's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub repo: String,
    /// The commit the run checked out: the only one it's signed for.
    pub head: String,
    pub tree: String,
    pub check: String,
    pub recipe: Recipe,
    pub passed: bool,
    /// `flow::receipt_digest` of the program's output.
    pub log: String,
}

/// A signed result, as its check run's output text carries it. Only a
/// [`Signer`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    text: String,
    check: String,
    passed: bool,
}

impl Signed {
    /// The statement and its signature line.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// The check run it's for.
    pub fn check(&self) -> &str {
        &self.check
    }
    /// The check run's conclusion: a proof's failure fails it, and a
    /// review or judgment is a success whatever it found, its receipt
    /// carrying that.
    pub fn conclusion(&self) -> &'static str {
        if self.passed { "success" } else { "failure" }
    }
}

/// One producer's key.
pub struct Signer {
    producer: String,
    key: SigningKey,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("producer", &self.producer)
            .finish_non_exhaustive()
    }
}

impl Signer {
    /// `producer`'s key, as the machine's producers file names it, from
    /// the systemd credential named `credential`: a file in
    /// `$CREDENTIALS_DIRECTORY` holding the 32-byte Ed25519 secret, which
    /// no other user may read.
    pub fn load(producer: &str, credential: &str) -> Result<Self, String> {
        Self::load_from(&|name| std::env::var_os(name), producer, credential)
    }

    /// [`Signer::load`], reading the environment through `env`. The only
    /// variable it reads is `CREDENTIALS_DIRECTORY`.
    fn load_from(
        env: &dyn Fn(&str) -> Option<OsString>,
        producer: &str,
        credential: &str,
    ) -> Result<Self, String> {
        use std::os::unix::fs::PermissionsExt;
        if producer.trim().is_empty() || producer.trim() != producer {
            return Err(format!("{producer:?} isn't a producer's name"));
        }
        let plain = !credential.is_empty()
            && credential != "."
            && credential != ".."
            && !credential.contains('/');
        if !plain {
            return Err(format!(
                "{credential:?} isn't a credential's name: a key comes only as a systemd credential"
            ));
        }
        let dir = env("CREDENTIALS_DIRECTORY").ok_or(
            "no $CREDENTIALS_DIRECTORY: a producer's key comes only as a systemd credential",
        )?;
        let dir = Path::new(&dir);
        if !dir.is_absolute() {
            return Err("$CREDENTIALS_DIRECTORY isn't an absolute path".into());
        }
        let path = dir.join(credential);
        let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("{credential}: {e}"))?;
        if !meta.is_file() {
            return Err(format!("{credential} isn't a file"));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "{credential} can be read by others than its owner, so it isn't this producer's alone"
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{credential}: {e}"))?;
        let secret: [u8; 32] = bytes
            .try_into()
            .map_err(|_| format!("{credential} isn't a 32-byte Ed25519 secret"))?;
        Ok(Self {
            producer: producer.into(),
            key: SigningKey::from_bytes(&secret),
        })
    }

    /// Sign a published review the producer checked against its record.
    pub fn review(&self, r: &Review) -> Signed {
        let log = flow::receipt_digest(&flow::review_receipt(&r.head, &r.body));
        self.sign(
            &r.repo,
            &r.head,
            &r.tree,
            &format!("conary/review/{}", r.role),
            None,
            true,
            log,
        )
    }

    /// Sign a judgment the producer issued.
    pub fn judgment(&self, j: &Judgment) -> Signed {
        let receipt =
            flow::judgment_receipt_by(&j.revision, j.generation, j.verdict, j.provenance.as_ref());
        let log = flow::receipt_digest(&receipt);
        self.sign(
            &j.repo,
            &j.revision.head,
            &j.tree,
            JUDGMENT,
            None,
            true,
            log,
        )
    }

    /// Sign a proof's recorded exit, for the commit it ran on.
    pub fn proof(&self, p: &Proof) -> Signed {
        let definition = flow::recipe_digest(&p.recipe);
        self.sign(
            &p.repo,
            &p.head,
            &p.tree,
            &p.check,
            Some(definition),
            p.passed,
            p.log.clone(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn sign(
        &self,
        repo: &str,
        head: &str,
        tree: &str,
        check: &str,
        definition: Option<String>,
        passed: bool,
        log: String,
    ) -> Signed {
        let statement = Statement {
            producer: self.producer.clone(),
            repository: repo.into(),
            revision: Revision {
                base: None,
                head: head.into(),
            },
            tree: Some(tree.into()),
            check: check.into(),
            definition,
            result: if passed { "pass" } else { "fail" }.into(),
            log: Log { sha256: log },
        };
        Signed {
            text: service::sign(&statement, &self.key),
            check: check.into(),
            passed,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(producer: &str, secret: [u8; 32]) -> Self {
        Self {
            producer: producer.into(),
            key: SigningKey::from_bytes(&secret),
        }
    }

    #[cfg(test)]
    pub(crate) fn load_with(
        env: &dyn Fn(&str) -> Option<OsString>,
        producer: &str,
        credential: &str,
    ) -> Result<Self, String> {
        Self::load_from(env, producer, credential)
    }
}
