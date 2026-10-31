# Which waiting turns take a slot now, and which slots: take_slot's choice
# (proj.sh), on its own so the flake can check it. It reads what take_slot
# sees, one tab-separated line each: "slot N free", or "slot N CLASS PROJECT"
# for one that's taken (its turn's class and project, "-" when not known), for
# every slot in order; then "wait NAME CLASS PROJECT" for every turn in line,
# the head first. CLASS is lead, pipeline, steward, first or normal. It hands out every
# free slot it can in one look, printing "NAME N" for each turn that may start,
# in the order they're chosen, or nothing; a turn that looks and finds its
# name starts, so free slots never wait on one named waiter's look.
#
# The last PIPELINE_SLOTS of the MAX_JOBS slots are for pipeline turns only
# (work on the pipeline itself, in its own repos' projects: class says), so
# one waits only on other pipeline turns, never on everyone else's long
# jobs; a pipeline turn takes the lowest free slot, which keeps those for the
# next one, unless a turn chosen after it needs that slot and only the
# pipeline's are left for it. A lead turn (pipeline work in a lead repo's
# project, what the pipeline is built from) is a pipeline turn that goes first of all: ahead
# of the whole line, the other pipeline turns included, except that it leaves
# them the pipeline's own slots while one waits, so a lead repo's backlog can't
# stop the work on what deploys it. Any other turn waits
# while its project holds PER_PROJECT slots (pipeline turns not counted), and
# lets those behind it by meanwhile, so one project's jobs never fill the
# machine. The last of a project's PER_PROJECT stays free for its steward turns
# (run --pipeline, outside the pipeline's own repos), so its own lanes can't
# hold up its gates and conflict fixes: only a steward turn starts when the
# project's other turns hold PER_PROJECT - 1, or the one slot when PER_PROJECT
# is 1, so that slot still goes to whoever's in line. Those other turns are
# every normal and first one, one that has waited into first and one run
# --first alike, and a steward turn may use the whole share.
#
# Everyone else's turns hold a slot whenever they have one that may start:
# until they do, a pipeline turn, lead or not, leaves them one free ordinary
# slot and takes what's left, so a backlog of pipeline work slows the others
# and never stops them.
pick() {
  local lanes=$((PER_PROJECT > 1 ? PER_PROJECT - 1 : 1))
  local kind a b c w pass t=0 r=0 l=0 q=0 others=0 owed=0 spare open need loose x=0 y=0 went=() ours=() theirs=() inline=()
  local -A held=() others_of=()
  while IFS=$'\t' read -r kind a b c; do
    case $kind in
      slot)
        if [ "$b" = free ]; then
          if [ "$a" -le $((MAX_JOBS - PIPELINE_SLOTS)) ]; then theirs+=("$a"); else ours+=("$a"); fi
        elif [ "$b" != pipeline ] && [ "$b" != lead ]; then
          held[p$c]=$((${held[p$c]:-0} + 1))
          [ "$b" = steward ] || others_of[p$c]=$((${others_of[p$c]:-0} + 1))
          others=$((others + 1))
        fi
        ;;
      wait)
        inline+=("$a"$'\t'"$b"$'\t'"$c")
        [ "$b" != pipeline ] || q=$((q + 1))
        ;;
    esac
  done
  if [ "$others" -eq 0 ]; then
    for w in "${inline[@]}"; do
      IFS=$'\t' read -r a b c <<<"$w"
      case $b in lead | pipeline) continue ;; esac
      within_share "$b" "$c" || continue
      owed=1
      break
    done
  fi
  # Who goes: each in turn, while the slots still go round. t turns chosen, l
  # of them lead and r held to the slots anyone may take; spare is how many of
  # those the slot owed to the others leaves pipeline turns; open is how many
  # of the pipeline's own slots the q other pipeline turns in line leave lead
  # ones, and need how many of the slots anyone may take lead ones must have.
  spare=$((${#theirs[@]} > owed ? ${#theirs[@]} - owed : 0))
  open=$((${#ours[@]} > q ? ${#ours[@]} - q : 0))
  for pass in lead rest; do
    for w in "${inline[@]}"; do
      IFS=$'\t' read -r a b c <<<"$w"
      if [ "$pass" = lead ]; then [ "$b" = lead ] || continue
      else [ "$b" != lead ] || continue
      fi
      [ "$t" -lt $((${#theirs[@]} + ${#ours[@]})) ] || continue
      case $b in
        lead)
          [ "$l" -lt $((spare + open)) ] || continue
          l=$((l + 1))
          went+=("$a"$'\t'"$b")
          ;;
        pipeline)
          [ $((t - r)) -lt $((spare + ${#ours[@]})) ] || continue
          went+=("$a"$'\t'"$b")
          ;;
        *)
          need=$((l > open ? l - open : 0))
          within_share "$b" "$c" && [ $((r + need)) -lt "${#theirs[@]}" ] || continue
          held[p$c]=$((${held[p$c]:-0} + 1))
          [ "$b" = steward ] || others_of[p$c]=$((${others_of[p$c]:-0} + 1))
          r=$((r + 1))
          went+=("$a"$'\t'"$b")
          ;;
      esac
      t=$((t + 1))
    done
  done
  # Which slot each takes, lowest first: pipeline turns may have only what the
  # others chosen, and the slot owed to them, leave them of the slots anyone
  # may take.
  loose=$((${#theirs[@]} - r < spare ? ${#theirs[@]} - r : spare))
  for w in "${went[@]}"; do
    a=${w%$'\t'*} b=${w##*$'\t'}
    case $b in
      lead | pipeline)
        if [ "$loose" -le 0 ]; then
          printf '%s\t%s\n' "$a" "${ours[y]}"
          y=$((y + 1))
          continue
        fi
        loose=$((loose - 1))
        ;;
    esac
    printf '%s\t%s\n' "$a" "${theirs[x]}"
    x=$((x + 1))
  done
}

# Whether a turn of this class may start in this project, by the counts pick
# keeps (its locals).
within_share() {
  [ "${held[p$2]:-0}" -lt "$PER_PROJECT" ] && { [ "$1" = steward ] || [ "${others_of[p$2]:-0}" -lt "$lanes" ]; }
}

# The slot pick (on stdin) gave the turn named, or nothing. Reads all of it, so
# pick never writes to a closed pipe.
picked() {
  local w s
  while IFS=$'\t' read -r w s; do
    if [ "$w" = "$1" ]; then echo "$s"; fi
  done
}

# A file's first line into the variable named, or nothing, with no fork: the
# line has a waiter's worth of these to read at every look.
line_of() {
  local -n into=$1
  into=''
  # shellcheck disable=SC2034 # the caller's
  { IFS= read -r into <"$2"; } 2>/dev/null || true
}

# A clone's origin URL, or nothing (no clone, no origin, or no path at all:
# git -C "" would ask whatever repo it runs in).
origin() {
  [ -n "$1" ] || return 0
  git -C "$1" remote get-url origin 2>/dev/null || true
}

# The GitHub repo a clone is of, as owner/name in lower case, from its origin
# in any form git takes (https, ssh:// or scp-like, a user or a .git or not),
# or nothing for anything else: another host, a local path, a longer path, a
# query, or a name GitHub wouldn't give (so what it prints is never more than
# a repo's name, whatever else the origin holds).
github_repo() {
  local url
  url=$(origin "$1")
  url=${url,,}
  url=${url%/}
  url=${url%.git}
  if [[ $url =~ ^(https?://([^@/]+@)?github\.com/|ssh://([^@/]+@)?github\.com(:[0-9]+)?/|([^@/]+@)?github\.com:)([a-z0-9-]+/[a-z0-9._-]+)$ ]]; then
    echo "${BASH_REMATCH[6]}"
  fi
}

# Whether a repo (github_repo's owner/name) is in a list of OWNER/NAMEs, any
# case.
listed() {
  [ -n "$1" ] || return 1
  case " ${2,,} " in *" $1 "*) return 0 ;; esac
  return 1
}

# Whether a path is the machine's flake (FLAKE), which deploys everything,
# wherever its origin is. It's the same directory however either path spells
# it (doubled or trailing slashes, dots, symlinks); a path that's empty or
# isn't there is never it.
flake_project() {
  local p f
  p=$(realpath -e -- "$1" 2>/dev/null) && f=$(realpath -e -- "$FLAKE" 2>/dev/null) && [ "$p" = "$f" ]
}

# Whether the project at a path is the pipeline's own: the flake, or a clone of
# one of the pipeline's own repos (PIPELINE: their OWNER/NAMEs, any case, as
# autoland.pipelineRepos and leadRepos have them).
pipeline_project() {
  flake_project "$1" || listed "$(github_repo "$1")" "$PIPELINE"
}

# What class has found of each project since classes was last emptied: asking
# means git, so a look at the line (look, line.sh) asks once a project.
declare -A classes=()

# A job's class, from its dir, into the_class. One run with --pipeline is a
# pipeline one, every turn of it, only while its project is one of the
# pipeline's own; anywhere else it's steward, every turn of it, so it still goes
# ahead of the line, with the first ones, and has the slot of its project's
# PER_PROJECT the others leave it, but counts against that PER_PROJECT and stays
# off the pipeline's slots (one repo's PRs, waiting on that repo's own lock,
# can't fill the machine). In a lead repo's project (LEAD: the ones among PIPELINE
# that go first of all, as autoland.leadRepos has them) it's lead. Asked each
# time, so a job from before this rule, or one whose repo was taken off a
# list since, is held to it too. Otherwise first or normal, as the turn was
# started.
the_class=''
class() {
  local d='' r
  if [ ! -e "$1/pipeline" ]; then the_class=${2:-normal}; return 0; fi
  line_of d "$1/project"
  if [ -z "${classes[p$d]:-}" ]; then
    r=$(github_repo "$d")
    if listed "$r" "$LEAD"; then classes[p$d]=lead
    elif flake_project "$d" || listed "$r" "$PIPELINE"; then classes[p$d]=pipeline
    else classes[p$d]=steward
    fi
  fi
  the_class=${classes[p$d]}
}

# A waiting turn's class now, into the_class, and into the_place the name its
# place in line (JOBS/.queue's PREFIX-WHEN-ID) should have for it: 0 lead or
# pipeline (pick takes lead ones first), 1 steward or first, 2 normal. Its prefix says
# what the turn itself is (2 normal, anything else first: run with --first, or
# waited past MAX_WAIT), and class what that makes it now, so a job whose
# origin or list changed while it waits moves in line and is held to its
# project's cap as what it is now.
# shellcheck disable=SC2034 # the caller's
the_place=''
place() {
  local p
  if [[ $1 == 2-* ]]; then class "$2" normal; else class "$2" first; fi
  case $the_class in lead | pipeline) p=0 ;; steward | first) p=1 ;; *) p=2 ;; esac
  # shellcheck disable=SC2034 # the caller's
  the_place=$p-${1#?-}
}

# What proj says of a job that asked to be pipeline work and isn't, so a
# mis-set origin or list doesn't quietly demote it: nothing for any other.
# Never the origin itself, which can hold a token: only the repo it names.
demoted() {
  local d r
  [ -e "$1/pipeline" ] || return 0
  d=$(cat "$1/project" 2>/dev/null) || d=''
  pipeline_project "$d" && return 0
  r=$(github_repo "$d")
  if [ -n "$r" ]; then echo "asked as --pipeline, but its origin, github.com/$r, isn't in pipelineRepos: it goes as a steward job"
  elif [ -n "$(origin "$d")" ]; then echo "asked as --pipeline, but its origin isn't a GitHub repo's: it goes as a steward job"
  else echo "asked as --pipeline, but its project has no origin: it goes as a steward job"
  fi
}
