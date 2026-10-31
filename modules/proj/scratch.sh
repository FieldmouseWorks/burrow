# Each job's scratch: SCRATCH/jobs/ID, its turns' PROJ_SCRATCH, where it makes
# its worktrees, clones and builds. It goes, with every worktree in it, once
# the job is over, so nothing a finished job built (cargo's target dirs run to
# gigabytes a worktree) is left to pile up. proj.sh's _ended looks at the end
# of each turn, and proj stop at a job it ends between turns; the job's own
# end is the event, never a timer. SCRATCH comes from ../proj.nix.

scratch_of() { echo "$SCRATCH/jobs/$1"; }

# Whether job ID is over as its latest turn left it, so nothing gives it
# another turn of its own accord: proj stop ended it, or it awaits nothing
# and either said `proj done` or is --owned (its starter acts on each turn's
# end itself, and starts a new job for anything more). One that awaits
# GitHub, asks a person, or ended any other way (a failure, no word on what
# comes next) keeps its scratch: what it awaits, or a reply, resumes it there.
# A reply to a job that was over finds its scratch made afresh, empty.
over() {
  local d=$JOBS/$1 n
  n=$(cat "$d/turns" 2>/dev/null) || return 1
  [ -n "$n" ] || return 1
  [ ! -e "$d/$n.stopped" ] || return 0
  [ ! -s "$d/await" ] || return 1
  [ "$(cat "$d/$n.end" 2>/dev/null)" = "done" ] || [ -e "$d/owned" ]
}

# Job ID's scratch gone, and each worktree in it taken off the repo it's a
# worktree of (git worktree prune), so its branch can be checked out again.
# It's renamed out of the way first, which only one of two at once can do,
# so a turn that starts next makes its own afresh and a second clear (the
# turn's end and proj stop, both) finds nothing to do. Only once no turn of
# the job runs: whatever still worked in it would lose its files.
scratch_clear() {
  local dir gone g common
  local -A repos=()
  dir=$(scratch_of "$1")
  [ -e "$dir" ] || return 0
  gone=$SCRATCH/jobs/.gone-$1-$$
  mv -T -- "$dir" "$gone" 2>/dev/null || return 0
  # A worktree's .git is a file naming its repo; a clone's own repo goes with it.
  while IFS= read -r -d '' g; do
    common=$(git -C "${g%/.git}" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || continue
    case $common in "$gone" | "$gone"/*) ;; *) repos[$common]=1 ;; esac
  done < <(find "$gone" -maxdepth 4 -name .git -type f -print0 2>/dev/null)
  # What a build left read-only (a Go module cache, a store path copied out)
  # stops rm until it's writable again.
  rm -rf -- "$gone" 2>/dev/null || { chmod -R u+w -- "$gone" 2>/dev/null; rm -rf -- "$gone"; } ||
    echo "proj: couldn't remove all of job $1's scratch, left in $gone" >&2
  for common in "${!repos[@]}"; do
    git --git-dir="$common" worktree prune || echo "proj: couldn't prune $common's worktrees after job $1" >&2
  done
}
