# Which waiting turn takes a slot now, and which slot: take_slot's choice
# (proj.sh), on its own so the flake can check it. It reads what take_slot
# sees, one tab-separated line each: "slot N free", or "slot N CLASS PROJECT"
# for one that's taken (its turn's class and project, "-" when not known), for
# every slot in order; then "wait NAME CLASS PROJECT" for every turn in line,
# the head first. CLASS is pipeline, first or normal. It prints "NAME N" for
# the first turn in line that may start, or nothing.
#
# The last PIPELINE_SLOTS of the MAX_JOBS slots are for pipeline turns only
# (work on the pipeline itself: the steward's jobs, its repos' issues), so
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
