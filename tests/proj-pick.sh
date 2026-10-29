# proj's choice of who takes a slot (modules/proj/pick.sh), on what take_slot
# would see: pipeline jobs' slots stay theirs, one project's jobs stop at
# PER_PROJECT while the others go by, and pipeline ones count against no cap;
# and which jobs are pipeline ones: only those in the pipeline's own repos.
#
#   bash tests/proj-pick.sh modules/proj/pick.sh
set -euo pipefail
# shellcheck source=/dev/null
. "$1"
export MAX_JOBS=4 PIPELINE_SLOTS=1 PER_PROJECT=2
T=$'\t'

# The slots, then the line: "free", or a holder's "CLASS PROJECT", for slots 1
# to 4 in order; then "NAME CLASS PROJECT" for each waiting turn.
case_() {
  local want=$1 i=0 s line='' got
  shift
  for s in "$@"; do
    # Fields split at their first spaces only: a project's own stay in it.
    s=${s/ /$T}
    if [ "$i" -lt "$MAX_JOBS" ]; then
      i=$((i + 1))
      line+="slot$T$i$T$s"$'\n'
    else
      line+="wait$T${s/ /$T}"$'\n'
    fi
  done
  got=$(printf '%s' "$line" | pick)
  [ "$got" = "${want// /$T}" ] || { echo "FAIL: wanted '$want', got '${got//$T/ }' from: $*" >&2; exit 1; }
}

# The head of the line takes the lowest free slot.
case_ "a 1" free free free free "a normal /src/web" "b normal /src/lok"
case_ "a 3" "normal /src/x" "normal /src/y" free free "a normal /src/web" "b first /src/lok"
# Nothing free: nobody goes.
case_ "" "normal /src/x" "normal /src/y" "normal /src/z" "pipeline /src/b" "a pipeline /src/b"
# The last slot is the pipeline's: nobody else takes it, and a pipeline turn
# behind them does.
case_ "" "normal /src/x" "normal /src/y" "normal /src/z" free "a normal /src/web"
case_ "p 4" "normal /src/x" "normal /src/y" "normal /src/z" free "a normal /src/web" "p pipeline /src/b"
# Holders proj can't name (from before it named them) are someone else's.
case_ "" "- -" "- -" "- -" free "a first /src/web"
# A pipeline turn takes the lowest free slot, leaving its own for the next.
case_ "p 2" "normal /src/x" free free free "p pipeline /src/b"
# A project holding PER_PROJECT slots waits, and lets the others by.
case_ "b 3" "normal /src/psiv" "first /src/psiv" free free "a normal /src/psiv" "b normal /src/lok"
case_ "" "normal /src/psiv" "normal /src/psiv" free free "a first /src/psiv"
# A pipeline repo's pipeline jobs don't count against it, and aren't held by it.
case_ "a 3" "pipeline /src/remi" "normal /src/remi" free free "a normal /src/remi"
case_ "p 3" "normal /src/remi" "normal /src/remi" free free "p pipeline /src/remi"
# Projects are told apart whole, spaces and all; a turn whose project can't be
# read is in none.
case_ "b 3" "normal /src/my web" "normal /src/my web" free free "a normal /src/my web" "b normal /src/my"
case_ "a 3" "normal /src/x" "normal /src/x" free free "a normal "

# A job run with --pipeline is a pipeline one only in a project of one of the
# pipeline's own repos, told by its clone's origin (any case, either URL
# form). Anywhere else it goes as --first, so pick holds it to its project's
# cap and off the pipeline's slots: one repo's PRs can't fill the machine.
export PIPELINE="Sam/Burrow sam/box"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export GIT_CEILING_DIRECTORIES=$tmp
clone() { git init -q "$tmp/$1" && git -C "$tmp/$1" remote add origin "$2"; }
clone burrow https://github.com/sam/burrow.git
clone box git@github.com:SAM/BOX
clone slash https://github.com/sam/burrow/
clone psiv https://github.com/sam/psiv
clone theirs https://github.com/kim/burrow
mkdir "$tmp/plain"
for d in burrow box slash; do
  pipeline_project "$tmp/$d" || { echo "FAIL: $d isn't the pipeline's" >&2; exit 1; }
done
for d in psiv theirs plain missing; do
  ! pipeline_project "$tmp/$d" || { echo "FAIL: $d is the pipeline's" >&2; exit 1; }
done

# What each turn of a job queues as: what proj run recorded for its
# --pipeline (pipeline, or first outside the pipeline's repos), whatever the
# turn asks; otherwise the turn's own.
mkdir -p "$tmp/jobs/p" "$tmp/jobs/f" "$tmp/jobs/n"
: >"$tmp/jobs/p/pipeline"
: >"$tmp/jobs/f/first"
cls() {
  local got
  got=$(class "$tmp/jobs/$1" "$2")
  [ "$got" = "$3" ] || { echo "FAIL: class $1 '$2': wanted $3, got $got" >&2; exit 1; }
}
cls p normal pipeline
cls p first pipeline
cls f normal first
cls f "" first
cls n first first
cls n normal normal
cls n "" normal

echo "proj-pick: ok"
