//! The signer (Conary's `docs/m6-plugins.md`, "One key per producer" and
//! "No signing verb"). A producer's Ed25519 key comes only from its
//! systemd credential, which only its own user can read: never from the
//! environment, an argument or a path. It signs only the producer's own
//! results, each from its durable [`Record`] ([`Review`], [`Judgment`],
//! [`Proof`]), as Conary's service statement (`attest::service`), with
//! Conary's receipts as the log, and only as [`crate::publish`] publishes
//! that record. Nothing public signs, and nothing takes a statement.
use crate::records::{Record, Target};
use conary_core::plugin::{Judgment as Verdict, Provenance};
use conary_github::attest::service::{self, Log, Revision, Statement};
use conary_github::flow::{self, Recipe};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::ffi::OsString;
use std::path::Path;

/// The check a producer's review, judgment or proof is published as.
const JUDGMENT: &str = "conary/judgment";

/// A result a producer can publish signed: a [`Review`], a [`Judgment`] or
/// a [`Proof`], and nothing else.
pub trait Signable: sealed::Signs + Serialize + DeserializeOwned + Clone {
    /// The check run it's published as.
    fn target(&self) -> Target;
}

mod sealed {
    /// How a result is signed. Its module is private, so only this crate
    /// can call it or add a kind of result.
    pub trait Signs {
        fn signed_by(&self, signer: &super::Signer) -> super::Signed;
    }
}

impl Signable for Review {
    fn target(&self) -> Target {
        Target {
            repo: self.repo.clone(),
            head: self.head.clone(),
            check: format!("conary/review/{}", self.role),
        }
    }
}

impl sealed::Signs for Review {
    fn signed_by(&self, signer: &Signer) -> Signed {
        let log = flow::receipt_digest(&flow::review_receipt(&self.head, &self.body));
        signer.sign(&self.target(), &self.tree, None, true, log)
    }
}

impl Signable for Judgment {
    fn target(&self) -> Target {
        Target {
            repo: self.repo.clone(),
            head: self.revision.head.clone(),
            check: JUDGMENT.into(),
        }
    }
}

impl sealed::Signs for Judgment {
    fn signed_by(&self, signer: &Signer) -> Signed {
        let receipt = flow::judgment_receipt_by(
            &self.revision,
            self.generation,
            self.verdict,
            self.provenance.as_ref(),
        );
        let log = flow::receipt_digest(&receipt);
        signer.sign(&self.target(), &self.tree, None, true, log)
    }
}

impl Signable for Proof {
    fn target(&self) -> Target {
        Target {
            repo: self.repo.clone(),
            head: self.head.clone(),
            check: self.check.clone(),
        }
    }
}

impl sealed::Signs for Proof {
    fn signed_by(&self, signer: &Signer) -> Signed {
        let definition = flow::recipe_digest(&self.recipe);
        signer.sign(
            &self.target(),
            &self.tree,
            Some(definition),
            self.passed,
            self.log.clone(),
        )
    }
}

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
/// [`Signer`] makes one, from a record, as it's published.
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

/// A file's POSIX access ACL, as the kernel keeps it in its extended
/// attributes.
const ACCESS_ACL: &str = "system.posix_acl_access";

/// Whether anyone but root and `me` may read a file with `mode`, owned by
/// `owner`, with the access ACL `acl` (its xattr, if it has one). systemd
/// hands a service its credential as root's file, mode 0400, with an ACL
/// entry granting the service's user read: the mode's group bits are then
/// the ACL's mask, and the ACL says who may read.
pub(crate) fn others_may_read(
    mode: u32,
    owner: u32,
    acl: Option<&[u8]>,
    me: u32,
) -> Result<bool, String> {
    const READ: u16 = 4;
    let ours = |uid: u32| uid == 0 || uid == me;
    if mode & 0o400 != 0 && !ours(owner) || mode & 0o004 != 0 {
        return Ok(true);
    }
    let Some(acl) = acl else {
        return Ok(mode & 0o040 != 0);
    };
    let bad = || "its ACL doesn't read as one".to_string();
    let (version, entries) = acl.split_first_chunk::<4>().ok_or_else(bad)?;
    if u32::from_le_bytes(*version) != 2 || entries.len() % 8 != 0 {
        return Err(bad());
    }
    let entries: Vec<(u16, u16, u32)> = entries
        .as_chunks::<8>()
        .0
        .iter()
        .map(|e| {
            (
                u16::from_le_bytes([e[0], e[1]]),
                u16::from_le_bytes([e[2], e[3]]),
                u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
            )
        })
        .collect();
    let mask = entries
        .iter()
        .find(|e| e.0 == 0x10)
        .map_or(u16::MAX, |e| e.1);
    Ok(entries.iter().any(|&(tag, perm, id)| {
        let reads = perm & READ != 0;
        match tag {
            // The owner, whom the mode already said.
            0x01 => false,
            // A named user, through the mask.
            0x02 => reads && mask & READ != 0 && !ours(id),
            // The owning group, or a named one, through the mask.
            0x04 | 0x08 => reads && mask & READ != 0,
            0x10 => false,
            0x20 => reads,
            _ => true,
        }
    }))
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
        use std::os::unix::fs::MetadataExt;
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
        let mut buf = vec![0u8; 4096];
        let acl = match rustix::fs::getxattr(&path, ACCESS_ACL, &mut buf[..]) {
            Ok(n) => buf.get(..n),
            Err(rustix::io::Errno::NODATA | rustix::io::Errno::NOTSUP) => None,
            Err(e) => return Err(format!("{credential}'s ACL: {e}")),
        };
        if others_may_read(
            meta.mode(),
            meta.uid(),
            acl,
            rustix::process::geteuid().as_raw(),
        )? {
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

    /// The signed result `record` holds, for publication as its target.
    pub(crate) fn signed<T: Signable>(&self, record: &Record<T>) -> Result<Signed, String> {
        let data = record
            .data()
            .ok_or_else(|| format!("record {} holds no result to sign", record.id()))?;
        if &data.target() != record.target() {
            return Err(format!(
                "record {} holds another target's result",
                record.id()
            ));
        }
        Ok(sealed::Signs::signed_by(data, self))
    }

    fn sign(
        &self,
        target: &Target,
        tree: &str,
        definition: Option<String>,
        passed: bool,
        log: String,
    ) -> Signed {
        let statement = Statement {
            producer: self.producer.clone(),
            repository: target.repo.clone(),
            revision: Revision {
                base: None,
                head: target.head.clone(),
            },
            tree: Some(tree.into()),
            check: target.check.clone(),
            definition,
            result: if passed { "pass" } else { "fail" }.into(),
            log: Log { sha256: log },
        };
        Signed {
            text: service::sign(&statement, &self.key),
            check: target.check.clone(),
            passed,
        }
    }

    /// A signer with a key the test hands it: only in this crate's tests,
    /// and in a crate's that names the `test-key` feature among its
    /// dev-dependencies, never in a build that runs.
    #[cfg(any(test, feature = "test-key"))]
    pub fn for_tests(producer: &str, secret: [u8; 32]) -> Self {
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
