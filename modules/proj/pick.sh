# Which waiting turn takes a slot now, and which slot: take_slot's choice
# (proj.sh), on its own so the flake can check it. It reads what take_slot
# sees, one tab-separated line each: "slot N free", or "slot N CLASS PROJECT"
# for one that's taken (its turn's class and project, "-" when not known), for
# every slot in order; then "wait NAME CLASS PROJECT" for every turn in line,
# the head first. CLASS is pipeline, first or normal. It prints "NAME N" for
# the first turn in line that may start, or nothing.
#
# The last PIPELINE_SLOTS of the MAX_JOBS slots are for pipeline turns only
# (work on the pipeline itself, in its own repos' projects: class says), so
# one waits only on other pipeline turns, never on everyone else's long
# jobs; a pipeline turn takes the lowest free slot, which keeps those for the
# next one. Any other turn waits while its project holds PER_PROJECT slots
# (pipeline turns not counted), and lets those behind it by meanwhile, so one
# project's jobs never fill the machine.
pick() {
  local kind a b c n free=()
  local -A held=()
  while IFS=$'\t' read -r kind a b c; do
    case $kind in
      slot)
        if [ "$b" = free ]; then free+=("$a")
        elif [ "$b" != pipeline ]; then held[p$c]=$((${held[p$c]:-0} + 1))
        fi
        ;;
      wait)
        [ "$b" = pipeline ] || [ "${held[p$c]:-0}" -lt "$PER_PROJECT" ] || continue
        for n in "${free[@]}"; do
          if [ "$b" = pipeline ] || [ "$n" -le $((MAX_JOBS - PIPELINE_SLOTS)) ]; then
            printf '%s\t%s\n' "$a" "$n"
            return
          fi
        done
        ;;
    esac
  done
}

# A clone's origin URL, or nothing (no clone, no origin, or no path at all:
# git -C "" would ask whatever repo it runs in).
origin() {
  [ -n "$1" ] || return 0
  git -C "$1" remote get-url origin 2>/dev/null || true
}

# The GitHub repo a clone is of, as owner/name in lower case, from its origin
# in any form git takes (https, ssh:// or scp-like, a user or a .git or not),
# or nothing for anything else: another host, a local path, a longer path.
github_repo() {
  local url
  url=$(origin "$1")
  url=${url,,}
  url=${url%/}
  url=${url%.git}
  if [[ $url =~ ^(https?://([^@/]+@)?github\.com/|ssh://([^@/]+@)?github\.com(:[0-9]+)?/|([^@/]+@)?github\.com:)([^/]+/[^/]+)$ ]]; then
    echo "${BASH_REMATCH[6]}"
  fi
}

# Whether the project at a path is a clone of one of the pipeline's own repos
# (PIPELINE: their OWNER/NAMEs, any case, as autoland.pipelineRepos has them).
pipeline_project() {
  local r
  r=$(github_repo "$1")
  [ -n "$r" ] || return 1
  case " ${PIPELINE,,} " in *" $r "*) return 0 ;; esac
  return 1
}

# A job's class, from its dir. One run with --pipeline is a pipeline one,
# every turn of it, only while its project is one of the pipeline's own;
# anywhere else it's first, every turn of it, so it still goes ahead of the
# line but counts against its project's PER_PROJECT and stays off the
# pipeline's slots (one repo's PRs, waiting on that repo's own lock, can't
# fill the machine). Asked each time, so a job from before this rule, or one
# whose repo was taken off the list since, is held to it too. Otherwise first
# or normal, as the turn was started.
class() {
  if [ ! -e "$1/pipeline" ]; then echo "${2:-normal}"
  elif pipeline_project "$(cat "$1/project" 2>/dev/null)"; then echo pipeline
  else echo first
  fi
}

# What proj says of a job that asked to be pipeline work and isn't, so a
# mis-set origin or list doesn't quietly demote it: nothing for any other.
demoted() {
  local d o
  [ -e "$1/pipeline" ] || return 0
  d=$(cat "$1/project" 2>/dev/null) || d=''
  pipeline_project "$d" && return 0
  o=$(origin "$d")
  echo "asked as --pipeline, but its origin (${o:-none}) isn't in pipelineRepos: it goes as --first"
}
