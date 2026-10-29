# proj's choice of who takes a slot (modules/proj/pick.sh), on what take_slot
# would see: pipeline jobs' slots stay theirs, one project's jobs stop at
# PER_PROJECT while the others go by, and pipeline ones count against no cap.
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
# Its pipeline jobs don't count against it, and aren't held by it.
case_ "a 3" "pipeline /src/psiv" "normal /src/psiv" free free "a normal /src/psiv"
case_ "p 3" "normal /src/psiv" "normal /src/psiv" free free "p pipeline /src/psiv"
# Projects are told apart whole, spaces and all; a turn whose project can't be
# read is in none.
case_ "b 3" "normal /src/my web" "normal /src/my web" free free "a normal /src/my web" "b normal /src/my"
case_ "a 3" "normal /src/x" "normal /src/x" free free "a normal "

echo "proj-pick: ok"
