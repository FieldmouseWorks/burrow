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
# pipeline's own repos, told by its clone's origin: on GitHub, in any form git
# takes, any case. Anywhere else it goes as --first, so pick holds it to its
# project's cap and off the pipeline's slots: one repo's PRs can't fill the
# machine.
export PIPELINE="Sam/Burrow sam/box"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export GIT_CEILING_DIRECTORIES=$tmp
clone() { git init -q "$tmp/$1" && git -C "$tmp/$1" remote add origin "$2"; }
repo() {
  local got
  got=$(github_repo "$tmp/$1")
  [ "$got" = "$2" ] || { echo "FAIL: github_repo $1: wanted '$2', got '$got'" >&2; exit 1; }
}
clone https https://github.com/sam/burrow.git
clone slash https://github.com/sam/burrow/
clone user https://sam@github.com/sam/burrow
clone ssh ssh://git@github.com/sam/burrow.git
clone port ssh://git@github.com:22/sam/burrow
clone scp git@github.com:SAM/BOX
clone psiv https://github.com/sam/psiv
clone theirs https://github.com/kim/burrow
clone gitlab https://gitlab.com/sam/burrow.git
clone lookalike https://github.com.example.net/sam/burrow
clone suffix https://notgithub.com/sam/burrow
clone mirror /srv/mirrors/sam/burrow.git
clone longer https://github.com/sam/burrow/extra
# Where a token can sit in an origin: what's read from one is a name, or none.
clone token https://x-access-token:SECRET@github.com/sam/psiv.git
clone query 'https://github.com/sam/psiv?token=SECRET'
clone inpath https://github.com/tok:SECRET@sam/psiv
clone foreign https://sam:SECRET@gitlab.com/sam/psiv.git
mkdir "$tmp/plain"
for d in https slash user ssh port; do repo "$d" sam/burrow; done
repo scp sam/box
repo psiv sam/psiv
repo token sam/psiv
for d in gitlab lookalike suffix mirror longer query inpath foreign plain missing; do repo "$d" ""; done
# No path at all is no clone, not the one it's run in.
got=$(cd "$tmp/https" && github_repo "")
[ -z "$got" ] || { echo "FAIL: github_repo '' read the clone it ran in: $got" >&2; exit 1; }
for d in https ssh scp; do
  pipeline_project "$tmp/$d" || { echo "FAIL: $d isn't the pipeline's" >&2; exit 1; }
done
for d in psiv theirs gitlab mirror plain missing; do
  ! pipeline_project "$tmp/$d" || { echo "FAIL: $d is the pipeline's" >&2; exit 1; }
done

# What each turn of a job queues as: pipeline for one run with --pipeline in
# one of the pipeline's projects, first for one anywhere else (asked each
# time, so a job from before the rule is held to it), whatever the turn
# asks; otherwise the turn's own.
for j in p d o n t f; do mkdir -p "$tmp/jobs/$j"; done
for j in p d o t f; do : >"$tmp/jobs/$j/pipeline"; done
echo "$tmp/https" >"$tmp/jobs/p/project"
echo "$tmp/psiv" >"$tmp/jobs/d/project"
echo "$tmp/https" >"$tmp/jobs/n/project"
echo "$tmp/token" >"$tmp/jobs/t/project"
echo "$tmp/foreign" >"$tmp/jobs/f/project"
cls() {
  local got
  got=$(class "$tmp/jobs/$1" "$2")
  [ "$got" = "$3" ] || { echo "FAIL: class $1 '$2': wanted $3, got $got" >&2; exit 1; }
}
cls p normal pipeline
cls p first pipeline
cls d normal first
cls d "" first
cls o normal first
cls n first first
cls n normal normal
cls n "" normal

# A waiting turn moves in line, and is admitted and recorded, as what its job
# is now: one whose project stopped being the pipeline's while it waited goes
# as first, and back.
plc() {
  local got
  got=$(place "$1" "$tmp/jobs/$2")
  [ "$got" = "$3$T$4" ] || { echo "FAIL: place $1 $2: wanted '$3 $4', got '${got//$T/ }'" >&2; exit 1; }
}
plc 2-17-p-1-x p pipeline 0-17-p-1-x
plc 0-17-d-1-x d first 1-17-d-1-x
plc 1-17-p-1-x p pipeline 0-17-p-1-x
plc 0-17-p-1-x p pipeline 0-17-p-1-x
plc 2-17-n-1-x n normal 2-17-n-1-x
plc 1-17-n-1-x n first 1-17-n-1-x

# And proj says so of the ones it demotes, by the repo the origin names and
# never the origin itself, which can hold a token; and of no other.
dem() {
  local got
  got=$(demoted "$tmp/jobs/$1")
  [ "$got" = "$2" ] || { echo "FAIL: demoted $1: wanted '$2', got '$got'" >&2; exit 1; }
}
for j in t f; do
  got=$(demoted "$tmp/jobs/$j")
  [[ ${got,,} != *secret* ]] || { echo "FAIL: demoted $j shows the origin's token: $got" >&2; exit 1; }
done
dem p ""
dem n ""
dem d "asked as --pipeline, but its origin, github.com/sam/psiv, isn't in pipelineRepos: it goes as --first"
dem t "asked as --pipeline, but its origin, github.com/sam/psiv, isn't in pipelineRepos: it goes as --first"
dem f "asked as --pipeline, but its origin isn't a GitHub repo's: it goes as --first"
dem o "asked as --pipeline, but its project has no origin: it goes as --first"

echo "proj-pick: ok"
