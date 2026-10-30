# Waiting for a slot: take_slot, which every turn starts with (proj.sh's
# _exec), and the memory it asks about. pick (pick.sh) says who goes next in
# the line. MAX_JOBS and MIN_FREE come from ../proj.nix, jobs from proj.sh.

# Memory to spare, in MiB: what the kernel says is available, plus what the
# ZFS ARC would give back (the kernel doesn't count it).
spare() {
  local kb arc
  kb=$(awk '/^MemAvailable:/ { print $2 }' /proc/meminfo)
  arc=0
  if [ -r /proc/spl/kstat/zfs/arcstats ]; then
    arc=$(awk '$1 == "size" { s = $3 } $1 == "c_min" { m = $3 } END { print (s > m ? int((s - m) / 1024) : 0) }' /proc/spl/kstat/zfs/arcstats)
  fi
  echo $(((kb + arc) / 1024))
}

# Wait in line for a slot, then hold it on fd $slot until this shell ends.
# The line is JOBS/.queue: one file per waiting turn, named so that sorting
# puts pipeline ones ahead, then --first ones, then the oldest, holding the
# waiting shell's pid. A slot is a lock on JOBS/.slots/N, and N.who says
# whose (class and project), so a turn that's killed frees its slot (and its
# place in line) by itself. pick (pick.sh) says who has a slot. The turn is
# first or normal, as it was started; its job's class (place, in pick.sh)
# is asked again every pass, and its place in line, its admission and its
# slot's N.who all go by that one answer. A job run --unstick waits in no
# line at all (unstick_slot).
take_slot() {
  local id=$1 n=$2 class=$3 me since seen got f fid pid i fd lk busy new LC_ALL=C
  if [ -e "$jobs/$id/unstick" ]; then
    unstick_slot "$id" "$n"
    return
  fi
  mkdir -p "$jobs/.queue" "$jobs/.slots"
  since=$(date +%s)
  case $class in first) me=1 ;; *) me=2 ;; esac
  me="$jobs/.queue/$me-$(date +%s%N)-$id"
  trap 'rm -f "$me" "${me%/*}/.${me##*/}.new"' EXIT
  trap 'exit 143' TERM INT
  while :; do
    # Waited long enough: among the --first ones now, by when it came.
    if [[ ${me##*/} == 2-* ]] && [ $(($(date +%s) - since)) -ge "$MAX_WAIT" ]; then
      mv -f "$me" "${me%/*}/1-${me##*/2-}" 2>/dev/null || true
      me="${me%/*}/1-${me##*/2-}"
    fi
    # Where it stands now, and in the journal if that changed while it waits.
    IFS=$'\t' read -r class new < <(place "${me##*/}" "$jobs/$id")
    if [ "$new" != "${me##*/}" ]; then
      if mv -f "$me" "${me%/*}/$new" 2>/dev/null; then echo "proj: it goes as $class now" >&2; fi
      me="${me%/*}/$new"
    fi
    # Published whole (a dot file, which the line skips, then renamed), so a
    # waiter clearing out dead places never sees it without its pid. Put
    # back if it went anyway.
    if [ ! -e "$me" ]; then
      echo "$$" >"${me%/*}/.${me##*/}.new"
      mv -f "${me%/*}/.${me##*/}.new" "$me"
    fi
    # One waiter looks at a time, and a slot is taken only under this lock,
    # so one it sees free stays free until it's done.
    exec {lk}>"$jobs/.slots/.lock"
    flock "$lk"
    seen='' busy=''
    for i in $(seq 1 "$MAX_JOBS"); do
      exec {fd}>"$jobs/.slots/$i"
      if flock -n "$fd"; then seen+="slot"$'\t'"$i"$'\t'"free"$'\n'
      else
        busy=1
        seen+="slot"$'\t'"$i"$'\t'"$(cat "$jobs/.slots/$i.who" 2>/dev/null || printf -- '-\t-')"$'\n'
      fi
      exec {fd}>&-
    done
    # The line, once the places of shells that are gone are cleared.
    for f in "$jobs/.queue"/*; do
      [ -e "$f" ] || continue
      fid=${f##*/}; fid=${fid#*-}; fid=${fid#*-}
      pid=$(cat "$f" 2>/dev/null) || continue
      if [ "$f" = "$me" ]; then
        seen+="wait"$'\t'"$f"$'\t'"$class"
        seen+=$'\t'"$(cat "$jobs/$id/project" 2>/dev/null)"$'\n'
      elif [ -n "$pid" ] && tr '\0' ' ' 2>/dev/null <"/proc/$pid/cmdline" | grep -qF -- " _exec $fid "; then
        seen+="wait"$'\t'"$f"$'\t'"$(place "${f##*/}" "$jobs/$fid" | cut -f1)"
        seen+=$'\t'"$(cat "$jobs/$fid/project" 2>/dev/null)"$'\n'
      else
        rm -f "$f"
      fi
    done
    got=$(printf '%s' "$seen" | pick | picked "$me")
    # Short of memory matters only while another job runs: with nothing
    # running, waiting frees nothing.
    if [ -n "$got" ] && { [ -z "$busy" ] || [ "$(spare)" -ge "$MIN_FREE" ]; }; then
      i=$got
      exec {slot}>"$jobs/.slots/$i"
      if flock -n "$slot"; then
        printf '%s\t%s\n' "$class" "$(cat "$jobs/$id/project")" >"$jobs/.slots/$i.who"
        exec {lk}>&-
        rm -f "$me"
        trap - EXIT TERM INT
        echo "$i" >"$jobs/$id/$n.slot"
        return
      fi
      exec {slot}>&-
    fi
    exec {lk}>&-
    sleep 2
  done
}

# A job run --unstick takes what the pipeline is stuck on, and that can be
# the line itself (pick handing out no slots, say, or every slot held by
# turns that wait on each other), so its turns never join it: they take a
# slot of their own past the MAX_JOBS the line shares, JOBS/.slots/unstick,
# and wait only for another --unstick turn to give it up. Nor do they wait
# for memory to spare: claude.slice, which the machine caps, holds this turn
# with the rest, and the job that finds why the machine is full can't wait
# for it to empty. Held on fd $slot, as take_slot's are.
unstick_slot() {
  local id=$1 n=$2
  mkdir -p "$jobs/.slots"
  exec {slot}>"$jobs/.slots/unstick"
  flock "$slot"
  printf 'unstick\t%s\n' "$(cat "$jobs/$id/project")" >"$jobs/.slots/unstick.who"
  echo unstick >"$jobs/$id/$n.slot"
}
