# Waiting in line for a slot (proj.sh's _exec), for a look at a time, and what
# a turn hands to the deployed proj.
#
# What a waiting turn decides with is the deployed proj's, not the one it
# started on: at its next look after a switch it execs that one, and picks up
# its place in line there, so a change to who goes first reaches everyone
# waiting and take_slot and pick are always one version. A running turn stays
# on its proj, but what it tells the stall watchdog is the deployed one's too
# (rewatch, proj.sh), so the format of a watch item has one owner. All that
# crosses versions is these: `proj _exec ID CLASS`, in the same pid, with the
# place left in JOBS/.queue under that pid and the look's lock held on fd 9
# until that proj is up; and `proj _rewatch ID...`. HANDOVER_FORMAT names them,
# and a proj that doesn't print the same one isn't handed anything (one from
# before this, or a switch back to it): the turn stays on the proj it has.
HANDOVER_FORMAT=1
DEPLOYED=/run/current-system/sw/bin/proj

# The deployed proj's store path, so the switch after this one can't move it
# from under what's run, if it isn't this one and can take a hand-over.
successor() {
  local d
  d=$(readlink -f "$DEPLOYED" || true)
  [ "$d" != "$self" ] && [ -x "$d" ] && [ "$("$d" _handover_format 2>/dev/null || true)" = "$HANDOVER_FORMAT" ] || return 1
  echo "$d"
}

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
# slot's N.who all go by that one answer.
take_slot() {
  local id=$1 n=$2 class=$3 me='' when seen got f fid pid i fd lk busy new deployed LC_ALL=C
  mkdir -p "$jobs/.queue" "$jobs/.slots"
  # The place a proj before this one left, if it handed over.
  for f in "$jobs/.queue"/*-"$id"; do
    if [ -e "$f" ] && [ "$(cat "$f" 2>/dev/null)" = "$$" ]; then me=$f; fi
  done
  if [ -z "$me" ]; then
    case $class in first) me=1 ;; *) me=2 ;; esac
    me="$jobs/.queue/$me-$(date +%s%N)-$id"
  fi
  # When it came, which its place keeps through a hand-over.
  when=${me##*/}; when=${when#*-}; when=${when%%-*}
  trap 'rm -f "$me" "${me%/*}/.${me##*/}.new"' EXIT
  trap 'exit 143' TERM INT
  while :; do
    # A switch since the last look: on with the deployed proj, in this place.
    if deployed=$(successor); then
      # Mid-exec its cmdline reads empty, and a look clearing out dead places
      # would take this one for gone: no look runs until it's through (_exec).
      exec 9>"$jobs/.slots/.lock"
      flock 9
      exec "$deployed" _exec "$id" "$3"
    fi
    # Waited long enough: among the --first ones now, by when it came.
    if [[ ${me##*/} == 2-* ]] && [ $(($(date +%s) - when / 1000000000)) -ge "$MAX_WAIT" ]; then
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
