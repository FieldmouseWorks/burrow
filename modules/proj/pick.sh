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

# Whether the project at a path is one of the pipeline's own repos'
# (PIPELINE: their OWNER/NAMEs, any case, as autoland.pipelineRepos has
# them), by its clone's origin.
pipeline_project() {
  local url r name owner
  url=$(git -C "$1" remote get-url origin 2>/dev/null) || return 1
  r=${url%/}
  r=${r%.git}
  name=${r##*/}
  r=${r%/*}
  owner=${r##*[/:]}
  [ -n "$name" ] && [ -n "$owner" ] || return 1
  case " ${PIPELINE,,} " in *" ${owner,,}/${name,,} "*) return 0 ;; esac
  return 1
}

# A job's class, from its dir: pipeline for one run with --pipeline in one of
# the pipeline's own projects, every turn of it; first for one run with
# --pipeline in any other, every turn of it, so it still goes ahead of the
# line but counts against its project's PER_PROJECT and stays off the
# pipeline's slots (one repo's PRs, waiting on that repo's own lock, can't
# fill the machine); otherwise first or normal, as the turn was started.
class() {
  if [ -e "$1/pipeline" ]; then echo pipeline
  elif [ -e "$1/first" ]; then echo first
  else echo "${2:-normal}"
  fi
}
