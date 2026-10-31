# How one run of a turn goes (proj.sh's _exec): its deadline and quiet
# (nanos, watch_run), what it's told (LONG, KILLED, NEXT), and what's left of
# it once it's over (cgroup, oom_kills, reap), then the turn's next run in the
# same conversation (again). On its own so proj.sh stays a size a reader can
# hold. It runs with proj.sh's variables: jobs, dir, n, id, waitdir.

# A duration as timeout(1) reads it (90, 1.5h, 0.1s, 1d; 0 for none), in
# nanoseconds, rounded up; fails on anything else. Past about 30 years it's
# held there, as timeout(1) holds one too long for it: neither ever comes.
nanos() {
  awk -v d="$1" 'BEGIN {
    if (d !~ /^([0-9]+\.?[0-9]*|\.[0-9]+)([eE][-+]?[0-9]+)?[smhd]?$/) exit 1
    u = substr(d, length(d)); per = 1
    if (u == "m") per = 60; else if (u == "h") per = 3600; else if (u == "d") per = 86400
    if (u ~ /[smhd]/) d = substr(d, 1, length(d) - 1)
    s = d * per * 1e9; i = int(s); if (i < s) i++
    if (i > 1e18) i = 1e18
    printf "%.0f\n", i
  }'
}

# How a job waits on its own long work, which its answer would stop.
LONG="A command that can outlast one Bash call (ten minutes), a long build or test run, goes to the background with its output and exit code written to a file, and you wait for that file in the foreground, in steps under ten minutes each; answer only once it's there."

# What a run killed for memory before it answered is told in the next.
KILLED="Your last run was killed for memory before you answered, and everything it had running went with it. Carry on from where you left off: see what had already finished before you run anything again."

# How a turn says what comes next: what resumes the job, if anything.
NEXT="\`proj await URL\` for each PR or issue you're still waiting on (the job is resumed, with this conversation, once one of them changes: checks finish, a review, comment or push, merged or closed), \`proj ask\` when your answer is a question only a person can settle (their reply resumes it), or \`proj done\` when the work is finished"

# This shell's unit's cgroup.
cgroup() { echo "/sys/fs/cgroup$(sed -n 's/^0:://p' /proc/self/cgroup)"; }

# How many of the unit's processes the kernel has killed for memory, past
# whichever cap: the job's own, its slice's or the machine's.
oom_kills() { awk '$1 == "oom_kill" { k = $2 } END { print k + 0 }' "$(cgroup)/memory.events" 2>/dev/null || echo 0; }

# Everything in this shell's unit but the shell: what a run left behind, in
# whatever process group, TERM first, then KILL. Fails if any of it is still
# there after that.
reap() {
  local cg p sig procs others
  cg=$(cgroup)
  for sig in TERM 0 0 0 0 0 0 0 0 0 KILL 0 0 0 0 0 0 0 0 0; do
    others=''
    mapfile -t procs <"$cg/cgroup.procs"
    for p in "${procs[@]}"; do
      [ "$p" = "$$" ] && continue
      others=1
      kill -"$sig" "$p" 2>/dev/null || true
    done
    [ -n "$others" ] || return 0
    sleep 1
  done
  return 1
}

# A run's watcher (pid $1, under timeout(1)), once a second until it's gone,
# sooner when its turn's deadline is closer: it stops the run (through that
# timeout(1)) once the deadline in $n.deadline has passed ($n.late), or once
# its stream has said nothing for the job's --idle ($n.quiet), and keeps the
# turn's wait in line ($n.wait, from waiting) for rewatch, telling the stall
# watchdog as one starts or ends. While one of its commands waits in line,
# the deadline moves on with the clock: waiting its turn for something
# shared isn't being stuck, and the watchdog holds a wait to STALL_QUEUED.
# Telling the watchdog goes on in the background, never waited for: its
# lock or a slow kick holds up neither the clock nor the end of the run.
watch_run() {
  local pid=$1 quiet=0 now last w was='' dl nap
  [ ! -s "$dir/idle" ] || quiet=$((($(nanos "$(cat "$dir/idle")") + 999999999) / 1000000000))
  last=$(date +%s%N)
  while kill -0 "$pid" 2>/dev/null; do
    now=$(date +%s%N)
    w=$(waiting "$waitdir")
    dl=$(cat "$dir/$n.deadline" 2>/dev/null || true)
    if [ -n "$w" ] && [ -n "$dl" ]; then
      dl=$((dl + now - last))
      echo "$dl" >"$dir/$n.deadline.new" && mv -f "$dir/$n.deadline.new" "$dir/$n.deadline"
    fi
    last=$now
    if [ "$w" != "$was" ]; then
      if [ -n "$w" ]; then printf '%s\n' "$w" >"$dir/$n.wait"; else rm -f "$dir/$n.wait"; fi
      was=$w
      rewatch "$id" &
    fi
    if [ -n "$dl" ] && [ "$now" -ge "$dl" ]; then
      : >"$dir/$n.late"
      kill "$pid" 2>/dev/null || true
      break
    fi
    if [ "$quiet" -gt 0 ] && [ $((now / 1000000000 - $(stat -c %Y "$dir/$n.jsonl"))) -ge "$quiet" ]; then
      : >"$dir/$n.quiet"
      kill "$pid" 2>/dev/null || true
      break
    fi
    nap=1
    if [ -n "$dl" ] && [ -z "$w" ] && [ $((dl - now)) -lt 1000000000 ]; then
      nap=0.$(printf %09d $((dl - now)))
    fi
    sleep "$nap"
  done
  if [ -n "$was" ]; then
    rm -f "$dir/$n.wait"
    rewatch "$id" &
  fi
}

# The turn's next run, in the same conversation and slot, is told $1, kept
# in its $n.$2.
again() {
  printf '%s\n' "$1" >"$dir/$n.$2"
  prompt=$dir/$n.$2
  # The next run's quiet counts from its start, and is its own.
  rm -f "$dir/$n.quiet"
  touch "$dir/$n.jsonl"
  # The last run's watcher telling the watchdog its wait was over (still
  # going, in the background) may have gone with the rest of it: told again.
  rewatch "$id" &
}
