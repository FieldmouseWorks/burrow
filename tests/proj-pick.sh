# proj's choice of who takes a slot (modules/proj/pick.sh), on what take_slot
# would see: pipeline jobs' slots stay theirs, one project's jobs stop at
# PER_PROJECT while the others go by, pipeline ones count against no cap, and
# lead ones go ahead of everyone but the slots owed to the others, every free
# slot going in one look; and which jobs are pipeline ones: only those
# in the pipeline's own repos and the machine's flake, and lead ones those in
# its lead repos.
#
#   bash tests/proj-pick.sh modules/proj/pick.sh
set -euo pipefail
# shellcheck source=/dev/null
. "$1"
export MAX_JOBS=4 PIPELINE_SLOTS=1 PER_PROJECT=2 OTHER_SLOTS=1
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
NL=$'\n'

# The head of the line takes the lowest free slot.
case_ "a 1${NL}b 2" free free free free "a normal /src/web" "b normal /src/lok"
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
# A lead turn goes first, from anywhere in the line: ahead of pipeline turns
# that came before it, and of everyone else.
case_ "l 1${NL}p 2${NL}a 3" free free free free "p pipeline /src/remi" "a first /src/web" "l lead /src/burrow"
case_ "l 3${NL}p 4" "normal /src/x" "normal /src/y" free free "p pipeline /src/remi" "l lead /src/burrow"
# But the pipeline's own slot stays the other pipeline turns' while one waits,
# so a lead repo's backlog can't stop the work on what deploys it: the lead
# turn waits for the next slot.
case_ "p 4" "normal /src/x" "normal /src/y" "normal /src/z" free "p pipeline /src/remi" "l lead /src/burrow"
case_ "p 4" "lead /src/burrow" "lead /src/burrow" "lead /src/burrow" free "l lead /src/burrow" "m lead /src/burrow" "p pipeline /src/remi"
# With none of those in line it takes the pipeline's slot as a pipeline turn
# does, counts against no cap, and holds none of its project's.
case_ "l 4" "normal /src/burrow" "normal /src/burrow" "normal /src/z" free "a normal /src/web" "l lead /src/burrow"
case_ "a 3" "lead /src/burrow" "normal /src/burrow" free free "a normal /src/burrow"
# With no slot free nobody goes, lead or not; of two lead turns, the first in
# line goes first.
case_ "" "normal /src/x" "normal /src/y" "normal /src/z" "pipeline /src/b" "l lead /src/burrow"
case_ "l 2${NL}m 3${NL}p 4" "lead /src/burrow" free free free "p pipeline /src/remi" "l lead /src/burrow" "m lead /src/burrow"
case_ "l 4" "lead /src/burrow" "normal /src/x" "normal /src/y" free "l lead /src/burrow" "m lead /src/burrow"

# Every free slot goes in one look, in line order, under the same rules. Lead
# first, then the line; a pipeline turn gives up a slot anyone may take when a
# turn behind it needs that one.
MAX_JOBS=6
case_ "l 1${NL}p 2${NL}q 6${NL}a 3" free free free "lead /src/burrow" "lead /src/burrow" free \
  "l lead /src/burrow" "p pipeline /src/remi" "q pipeline /src/remi" "a normal /src/web"
# Slots ahead of what the line can fill stay free, the pipeline's above all.
case_ "a 1${NL}p 2" free free free free free free "a normal /src/web" "p pipeline /src/remi"
# More waiting than free: the head of the line goes, and those behind wait.
case_ "a 1${NL}b 2" free free "normal /src/x" "normal /src/y" "normal /src/z" "pipeline /src/b" \
  "a normal /src/web" "b normal /src/lok" "c normal /src/psiv"
# Nobody but pipeline turns takes the pipeline's slot, however long the line.
case_ "a 5" "normal /src/x" "normal /src/y" "normal /src/z" "first /src/z" free free \
  "a normal /src/web" "b normal /src/lok"
# A pipeline turn ahead of them takes it when that lets one of them go too.
case_ "p 6${NL}a 5" "normal /src/x" "normal /src/y" "normal /src/z" "first /src/z" free free \
  "p pipeline /src/remi" "a normal /src/web" "b normal /src/lok"
# PER_PROJECT counts the slots this look hands out: a project stops at its cap
# and those behind it go by.
case_ "a 1${NL}b 2${NL}d 3" free free free free free free \
  "a normal /src/web" "b first /src/web" "c normal /src/web" "d normal /src/lok"
case_ "a 1${NL}d 2" free free "normal /src/web" free free free \
  "a normal /src/web" "c normal /src/web" "d normal /src/lok"
# The pipeline's slot stays the other pipeline turns' whoever is ahead of them:
# a turn behind the lead one gets no slot that would push it onto theirs.
MAX_JOBS=6
case_ "l 3${NL}p 6" "normal /src/x" "normal /src/y" free "normal /src/z" "first /src/z" free \
  "l lead /src/burrow" "a normal /src/web" "p pipeline /src/remi"
# With two of them and one such turn, a lead one has the other in the same look.
PIPELINE_SLOTS=2
case_ "l 5${NL}p 6" "normal /src/x" "normal /src/y" "normal /src/z" "first /src/z" free free \
  "l lead /src/burrow" "m lead /src/burrow" "p pipeline /src/remi"
PIPELINE_SLOTS=1
MAX_JOBS=4
# A waiter finds its own slot in what pick says, or nothing.
assigned="a 1${NL}/q/2-1-b 3"
assigned=${assigned// /$T}
[ "$(picked /q/2-1-b <<<"$assigned")" = 3 ] || { echo "FAIL: picked didn't find its slot" >&2; exit 1; }
[ -z "$(picked /q/2-1 <<<"$assigned")" ] || { echo "FAIL: picked found another's slot" >&2; exit 1; }
[ -z "$(picked a </dev/null)" ] || { echo "FAIL: picked found a slot in nothing" >&2; exit 1; }

# A backlog of pipeline work doesn't starve the others: with none of theirs
# running, a pipeline or lead turn leaves them the last free ordinary slot.
case_ "a 3" "pipeline /src/remi" "lead /src/burrow" free "lead /src/burrow" "p pipeline /src/remi" "l lead /src/burrow" "a first /src/web"
case_ "a 3" "pipeline /src/remi" "lead /src/burrow" free "lead /src/burrow" "l lead /src/burrow" "a normal /src/web"
# It takes the pipeline's own slot instead, where one's free.
case_ "p 4${NL}a 3" "pipeline /src/remi" "lead /src/burrow" free free "p pipeline /src/remi" "a first /src/web"
# With more free than that it takes the lowest as ever, and the others still
# have theirs next.
case_ "l 2${NL}a 3" "pipeline /src/remi" free free "lead /src/burrow" "l lead /src/burrow" "a first /src/web"
# Both rules at once: a lead turn can't have the slot kept for the others, nor
# the pipeline's own while a pipeline turn waits, so that turn takes it; and
# with none waiting, the lead turn takes it.
case_ "p 4${NL}a 3" "pipeline /src/remi" "lead /src/burrow" free free "l lead /src/burrow" "p pipeline /src/remi" "a first /src/web"
case_ "l 4${NL}a 3" "pipeline /src/remi" "lead /src/burrow" free free "l lead /src/burrow" "a first /src/web"
# Nobody of theirs waiting: nothing's kept, and the pipeline takes the last.
case_ "p 3" "pipeline /src/remi" "lead /src/burrow" free "lead /src/burrow" "p pipeline /src/remi"
# One of theirs already running: that's their share, and the pipeline goes first.
case_ "p 3" "normal /src/x" "lead /src/burrow" free "lead /src/burrow" "p pipeline /src/remi" "a first /src/web"
# A holder proj can't name is someone else's here too.
case_ "p 3" "- -" "lead /src/burrow" free "lead /src/burrow" "p pipeline /src/remi" "a first /src/web"
# A turn its project's cap holds back can't use what's kept, so nothing is.
(
  PER_PROJECT=0
  case_ "p 3" "pipeline /src/remi" "lead /src/burrow" free "lead /src/burrow" "a first /src/web" "p pipeline /src/remi"
)
# In one look too, whoever is ahead in the line: pipeline turns stop at what
# the slots owed leave them, the pipeline's own included, and the others go.
(
  MAX_JOBS=6
  case_ "p 1${NL}q 6${NL}a 2" free free "lead /src/burrow" "lead /src/burrow" "lead /src/burrow" free \
    "p pipeline /src/remi" "q pipeline /src/remi" "r pipeline /src/remi" "a normal /src/web"
  case_ "l 1${NL}a 2" free free "lead /src/burrow" "lead /src/burrow" "lead /src/burrow" "pipeline /src/remi" \
    "l lead /src/burrow" "m lead /src/burrow" "a normal /src/web"
)
# Owing more than one of theirs can use, the pipeline still leaves what's owed.
(
  OTHER_SLOTS=2
  case_ "p 1${NL}q 4${NL}a 2" free free free free \
    "p pipeline /src/remi" "q pipeline /src/remi" "r pipeline /src/remi" "a normal /src/web"
)
# With no slots owed, pipeline work goes ahead of everything, as it's asked.
(
  OTHER_SLOTS=0
  case_ "p 3" "pipeline /src/remi" "lead /src/burrow" free "lead /src/burrow" "p pipeline /src/remi" "a first /src/web"
)

# A job run with --pipeline is a pipeline one only in a project of one of the
# pipeline's own repos, told by its clone's origin: on GitHub, in any form git
# takes, any case. Anywhere else it goes as --first, so pick holds it to its
# project's cap and off the pipeline's slots: one repo's PRs can't fill the
# machine.
export PIPELINE="Sam/Burrow sam/box" LEAD="" FLAKE=/nowhere
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

# A lead repo's pipeline jobs (LEAD, among PIPELINE, any case) are lead ones,
# in the same place in line as pipeline ones (pick takes them first); its
# other jobs, and pipeline ones elsewhere, are as they were.
LEAD="sam/BURROW"
mkdir -p "$tmp/jobs/b"
: >"$tmp/jobs/b/pipeline"
echo "$tmp/scp" >"$tmp/jobs/b/project"
cls p normal lead
cls p first lead
cls b normal pipeline
cls d normal first
cls n normal normal
plc 2-17-p-1-x p lead 0-17-p-1-x
plc 0-17-b-1-x b pipeline 0-17-b-1-x
dem p ""

# The machine's own flake is the pipeline's whatever its origin, and with no
# list at all: the update job's fixes there take the pipeline's slot with the
# others all taken and the flake's project at its cap. Any other project's
# --pipeline jobs, and the flake's plain ones, still aren't.
clone homelab https://github.com/sam/homelab
mkdir -p "$tmp/jobs/u" "$tmp/jobs/v" "$tmp/jobs/w"
: >"$tmp/jobs/u/pipeline"
: >"$tmp/jobs/v/pipeline"
echo "$tmp/homelab" >"$tmp/jobs/u/project"
echo "$tmp/psiv" >"$tmp/jobs/v/project"
echo "$tmp/homelab" >"$tmp/jobs/w/project"
PIPELINE='' FLAKE=$tmp/homelab/
cls u normal pipeline
cls v normal first
cls w normal normal
dem u ""
case_ "u 4" "normal $tmp/homelab" "normal $tmp/homelab" "normal $tmp/x" free \
  "w $(class "$tmp/jobs/w" normal) $tmp/homelab" "v $(class "$tmp/jobs/v" normal) $tmp/psiv" "u $(class "$tmp/jobs/u" normal) $tmp/homelab"

# The flake is the same directory however either side spells it: doubled and
# trailing slashes, dots, a symlink to it. Its --pipeline job takes the
# pipeline's slot by any of them; its plain job, and another project's, still
# don't.
ln -s homelab "$tmp/flake-link"
for flake in "$tmp/homelab" "$tmp//homelab/" "$tmp/./homelab" "$tmp/jobs/../homelab" "$tmp/flake-link" "$tmp/flake-link/"; do
  for proj in "$tmp//homelab" "$tmp/homelab/" "$tmp/./homelab" "$tmp/flake-link"; do
    echo "$proj" >"$tmp/jobs/u/project"
    echo "$proj" >"$tmp/jobs/w/project"
    PIPELINE='' LEAD='' FLAKE=$flake
    cls u normal pipeline
    cls v normal first
    cls w normal normal
    dem u ""
    case_ "u 4" "normal $proj" "normal $proj" "normal $tmp/x" free \
      "w $(class "$tmp/jobs/w" normal) $proj" "v $(class "$tmp/jobs/v" normal) $tmp/psiv" "u $(class "$tmp/jobs/u" normal) $proj"
  done
done

# Nothing that isn't there is the flake: not an empty path on either side,
# nor one that doesn't exist, even spelled the same.
mkdir -p "$tmp/jobs/e"
: >"$tmp/jobs/e/pipeline"
for pair in "$tmp/gone|$tmp/gone" "|" "|$tmp/homelab" "$tmp/homelab|"; do
  printf '%s\n' "${pair#*|}" >"$tmp/jobs/e/project"
  PIPELINE='' LEAD='' FLAKE=${pair%%|*}
  cls e normal first
done

echo "proj-pick: ok"
