# Each job's scratch (modules/proj/scratch.sh): when a job is over, so its
# scratch goes (said `proj done`, or stopped, or an --owned job's turn that
# awaits nothing; never one that awaits or asks, and only as its latest turn
# left it), and that clearing it takes its worktrees off their repos too, so
# their branches check out again, leaves every other job's alone, gets past
# what a build left read-only, and lets two clears at once both succeed.
#
#   bash tests/proj-scratch.sh modules/proj/scratch.sh
set -euo pipefail
# shellcheck source=/dev/null
. "$1"
tmp=$(mktemp -d)
trap 'chmod -R u+w "$tmp" 2>/dev/null; rm -rf "$tmp"' EXIT
export JOBS=$tmp/jobs SCRATCH=$tmp/scratch
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
fail() { echo "FAIL: $*" >&2; exit 1; }

# A job as its latest turn left it: "turns N", "N.end KIND", "N.stopped",
# "owned", "await" (a line of it), one argument each.
job() {
  local id=$1 f
  shift
  rm -rf "${JOBS:?}/$id"
  mkdir -p "$JOBS/$id"
  for f in "$@"; do
    case $f in
      turns*) echo "${f#turns }" >"$JOBS/$id/turns" ;;
      *.end*) echo "${f#* }" >"$JOBS/$id/${f%% *}" ;;
      await) printf 'o/r#1\t1000\t{}\n' >"$JOBS/$id/await" ;;
      *) : >"$JOBS/$id/$f" ;;
    esac
  done
}
is_over() { job j "$@"; over j || fail "wanted over: $*"; }
not_over() { job j "$@"; ! over j || fail "wanted not over: $*"; }

not_over
# Said it's done: over. Waiting on GitHub or on a person, or ended any other
# way (a failure, no word on what comes next): a reply or what it awaits
# resumes it, in its scratch.
is_over "turns 1" "1.end done"
not_over "turns 1" "1.end await" await
not_over "turns 1" "1.end ask"
not_over "turns 1" "1.end undeclared"
not_over "turns 1"
# The latest turn's word is what counts.
not_over "turns 2" "1.end done" "2.end ask"
is_over "turns 2" "1.end ask" "2.end done"
# --owned: its starter starts a new job for anything more, so a turn that
# awaits nothing ends it, however it ended; one that awaits doesn't.
is_over "turns 1" owned
is_over "turns 3" owned "3.end ask"
not_over "turns 1" owned "1.end await" await
# Stopped: over, whatever it was waiting on.
is_over "turns 1" "1.end await" await "1.stopped"
is_over "turns 2" "2.end ask" "2.stopped"
# A stop marks the turn it stopped: an earlier one's mark is history.
not_over "turns 2" "1.stopped" "2.end ask"

# A repo, and two jobs with worktrees of it in their scratch, one with a
# clone of its own as well.
git init -q -b main "$tmp/web"
git -C "$tmp/web" commit -q --allow-empty -m one
mkdir -p "$SCRATCH/jobs/a" "$SCRATCH/jobs/b"
git -C "$tmp/web" worktree add -q -b fix-a "$SCRATCH/jobs/a/web" main
git -C "$tmp/web" worktree add -q -b fix-b "$SCRATCH/jobs/b/web" main
git clone -q "$tmp/web" "$SCRATCH/jobs/a/clone"
# What a build leaves: gigabytes of it, some read-only.
mkdir -p "$SCRATCH/jobs/a/web/target/debug/ro"
head -c 1M /dev/zero >"$SCRATCH/jobs/a/web/target/debug/ro/lib.rlib"
chmod 0555 "$SCRATCH/jobs/a/web/target/debug/ro"

scratch_clear a
[ ! -e "$SCRATCH/jobs/a" ] || fail "job a's scratch is still there"
left=$(find "$SCRATCH/jobs" -mindepth 1 -maxdepth 1 -name '.gone-*')
[ -z "$left" ] || fail "left behind: $left"
# Its worktree is off the repo, so its branch checks out again; b's is kept.
trees=$(git -C "$tmp/web" worktree list --porcelain | sed -n 's/^worktree //p')
[ "$trees" = "$tmp/web"$'\n'"$SCRATCH/jobs/b/web" ] || fail "worktrees left: $trees"
git -C "$tmp/web" worktree add -q "$tmp/again" fix-a || fail "fix-a can't be checked out again"
[ -e "$SCRATCH/jobs/b/web/.git" ] || fail "job b's worktree went with a's"

# A job with no scratch (a sandbox's, or one cleared already): nothing to do.
scratch_clear a || fail "a second clear failed"
scratch_clear never || fail "clearing a job that never had scratch failed"

# The turn's end and proj stop clearing at once: both succeed, once.
for _ in 1 2 3 4 5; do
  git -C "$tmp/web" worktree add -q -b "race-$_" "$SCRATCH/jobs/c/web" main
  mkdir -p "$SCRATCH/jobs/c/web/target"
  seq 2000 | while read -r i; do : >"$SCRATCH/jobs/c/web/target/f$i"; done
  scratch_clear c & p=$!
  scratch_clear c || fail "one of two clears at once failed"
  wait "$p" || fail "the other of two clears at once failed"
  [ ! -e "$SCRATCH/jobs/c" ] || fail "c's scratch survived two clears"
  [ "$(git -C "$tmp/web" worktree list | wc -l)" = 3 ] || fail "a raced worktree is still listed"
done
left=$(find "$SCRATCH/jobs" -mindepth 1 -maxdepth 1 -name '.gone-*')
[ -z "$left" ] || fail "left behind after racing: $left"

echo "proj-scratch: ok"
