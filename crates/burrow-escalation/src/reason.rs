//! Why something may wait on the owner, typed: the only reasons there are.
//! Anything a producer can't name as one of these goes to the operator, who
//! acts under the owner's standing authority.

use serde::Serialize;

/// The one-way doors only the owner opens. A machine's flake may turn some
/// off (`burrow.escalation.ownerReasons`); it can't add one, since there's
/// no other value to add.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(into = "&'static str")]
pub enum Kind {
    /// Creating a secret.
    SecretCreate,
    /// Rotating one.
    SecretRotate,
    /// Deleting one.
    SecretDelete,
    /// Destroying or rolling back a ZFS dataset.
    ZfsDestroyOrRollback,
    /// Wiping or partitioning a disk.
    DiskWipeOrPartition,
    /// Deleting remote data (an rclone delete, purge or sync; a repo delete).
    RemoteDataDelete,
    /// Force-pushing a default branch.
    DefaultBranchForcePush,
    /// Flashing firmware.
    FirmwareFlash,
    /// The hosting provider's calls that cancel or reinstall the server.
    HostingCancelOrReinstall,
    /// A sign-in only a person can do, at a prompt that's still there.
    SignIn,
    /// A second-factor code only the owner has.
    TwoFactor,
    /// GitHub's "Confirm access" (sudo mode) on a settings page.
    ConfirmAccess,
}

/// The family a kind belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Secret,
    Irreversible,
    HumanPrompt,
}

impl Kind {
    /// Every kind, in the order the settings list them.
    pub const ALL: [Kind; 12] = [
        Kind::SecretCreate,
        Kind::SecretRotate,
        Kind::SecretDelete,
        Kind::ZfsDestroyOrRollback,
        Kind::DiskWipeOrPartition,
        Kind::RemoteDataDelete,
        Kind::DefaultBranchForcePush,
        Kind::FirmwareFlash,
        Kind::HostingCancelOrReinstall,
        Kind::SignIn,
        Kind::TwoFactor,
        Kind::ConfirmAccess,
    ];

    /// Its word in the settings, the CLI and the records.
    pub const fn word(self) -> &'static str {
        match self {
            Kind::SecretCreate => "secret-create",
            Kind::SecretRotate => "secret-rotate",
            Kind::SecretDelete => "secret-delete",
            Kind::ZfsDestroyOrRollback => "zfs-destroy-or-rollback",
            Kind::DiskWipeOrPartition => "disk-wipe-or-partition",
            Kind::RemoteDataDelete => "remote-data-delete",
            Kind::DefaultBranchForcePush => "default-branch-force-push",
            Kind::FirmwareFlash => "firmware-flash",
            Kind::HostingCancelOrReinstall => "hosting-cancel-or-reinstall",
            Kind::SignIn => "sign-in",
            Kind::TwoFactor => "two-factor",
            Kind::ConfirmAccess => "confirm-access",
        }
    }

    /// What it is, for the owner to read.
    pub const fn says(self) -> &'static str {
        match self {
            Kind::SecretCreate => "creating a secret",
            Kind::SecretRotate => "rotating a secret",
            Kind::SecretDelete => "deleting a secret",
            Kind::ZfsDestroyOrRollback => "destroying or rolling back a ZFS dataset",
            Kind::DiskWipeOrPartition => "wiping or partitioning a disk",
            Kind::RemoteDataDelete => "deleting remote data",
            Kind::DefaultBranchForcePush => "force-pushing a default branch",
            Kind::FirmwareFlash => "flashing firmware",
            Kind::HostingCancelOrReinstall => "cancelling or reinstalling the server",
            Kind::SignIn => "a sign-in only a person can do",
            Kind::TwoFactor => "a two-factor code",
            Kind::ConfirmAccess => "GitHub's Confirm access prompt",
        }
    }

    pub const fn family(self) -> Family {
        match self {
            Kind::SecretCreate | Kind::SecretRotate | Kind::SecretDelete => Family::Secret,
            Kind::SignIn | Kind::TwoFactor | Kind::ConfirmAccess => Family::HumanPrompt,
            _ => Family::Irreversible,
        }
    }

    pub fn parse(word: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.word() == word)
    }
}

impl From<Kind> for &'static str {
    fn from(kind: Kind) -> &'static str {
        kind.word()
    }
}

/// Every kind's word: what `escalation.ownerReasons` may list.
pub const WORDS: [&str; 12] = {
    let mut words = [""; 12];
    let mut i = 0;
    while i < 12 {
        words[i] = Kind::ALL[i].word();
        i += 1;
    }
    words
};

/// A reason that qualified: its kind, and what exactly it's about (which
/// secret, which dataset, which prompt on which site).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reason {
    pub kind: Kind,
    pub detail: String,
}

/// Where a request goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// The owner: the owner label, once their project has it.
    Owner(Reason),
    /// The operator, under the owner's standing authority; why it isn't the
    /// owner's.
    Operator(String),
}

/// Which reasons the machine lets reach the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub owner_reasons: Vec<Kind>,
    /// Whether a route to the owner's project is configured: with none, an
    /// owner label could never be delivered, so nothing goes to the owner and
    /// the operator raises it instead.
    pub delivers: bool,
}

/// The most a detail says: a line or two, never a document.
const DETAIL_MAX: usize = 500;

impl Policy {
    /// Where a request a producer names by `word`, about `detail`, goes.
    /// Only a kind the policy lists, with a detail saying what it's about,
    /// reaches the owner; everything else is the operator's.
    pub fn route(&self, word: &str, detail: &str) -> Route {
        let Some(kind) = Kind::parse(word.trim()) else {
            return Route::Operator(format!(
                "{word:?} isn't a one-way door ({}): the operator decides it under the standing authority",
                WORDS.join(", ")
            ));
        };
        if !self.owner_reasons.contains(&kind) {
            return Route::Operator(format!(
                "{} isn't one of this machine's owner reasons: the operator decides it",
                kind.word()
            ));
        }
        let detail = detail.trim();
        if detail.is_empty() {
            return Route::Operator(format!(
                "{} names nothing (which secret, dataset or prompt): the operator checks it",
                kind.word()
            ));
        }
        if detail.chars().count() > DETAIL_MAX {
            return Route::Operator(format!(
                "{}'s detail is longer than {DETAIL_MAX} characters: the operator checks it",
                kind.word()
            ));
        }
        if !self.delivers {
            return Route::Operator(format!(
                "{} is the owner's, but no route to their project is configured: the operator raises it",
                kind.word()
            ));
        }
        Route::Owner(Reason {
            kind,
            detail: detail.to_owned(),
        })
    }
}
