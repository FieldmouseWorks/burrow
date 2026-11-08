"""Where a PR is in the pipeline: the stage every status the steward posts
names (step.py's settle, which takes no status without one), each with its
label (labels.waiting in labels.nix, which says what each means; prs.py's
show_waiting puts it on). A PR carries its stage's label and none of the
others, so the PR itself and its repo's list say where it is, and its status
says the rest: who has it, until when, how many are ahead.

A PR that's out of the pipeline and marked so already, a draft (GitHub's
draft mark) or one on hold (the hold label), carries NONE's, which is none,
whatever stage its status names (settle sees to that)."""

REVIEW = "review"  # its reviewers, or a judge, look at this head
REVIEWER = "reviewer"  # a reviewer it waits on is out of service
FIX = "fix"  # a Claude job or its session fixes findings, red CI or a conflict
CI = "ci"  # reviewed clean, CI still running
PROOF_SLOT = "proofSlot"  # in line for the machine's own checks
PROOF = "proof"  # the machine's own checks building or running
RELEASE_AGE = "releaseAge"  # a lock refresh with nothing old enough yet
TRAIN = "train"  # in its repo's merge train, or held out of it
MERGE = "merge"  # settled: the steward, GitHub or its bot merges it
OWNER = "owner"  # the owner's to decide or settle
BASE = "base"  # waits on its base: the PR below, or a red default branch's CI
NONE = None

ALL = (REVIEW, REVIEWER, FIX, CI, PROOF_SLOT, PROOF, RELEASE_AGE, TRAIN, MERGE, OWNER, BASE)
