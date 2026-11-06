# heavy's slots: one holder at a time per slot, its share of the
# threads (all of them with --all), nested calls let through, the exit status and stdin passed
# on, a cancelled workload stopped before its slot goes, a bounded
# wait refused on time; and the example's gate slice weighted.
{ self, pkgs, ... }:
let
  heavy = (self.nixosConfigurations.example.extendModules {
    modules = [{ burrow.cpu.heavy = { slots = 1; grace = 3; dir = "/build/heavy"; }; }];
  }).config.burrow.tools.heavy;
  weight = self.nixosConfigurations.example.config.systemd.slices.ci.sliceConfig.CPUWeight;
  # A gate slice with a dash in its name would be nested: refused.
  nested = (builtins.tryEval (builtins.deepSeq
    (self.nixosConfigurations.example.extendModules { modules = [{ burrow.cpu.gates.slices = [ "nix-builds" ]; }]; }).config.burrow.cpu.gates.slices
    true)).success;
in pkgs.runCommand "cpu-check" { } ''
  [ ${toString weight} = 400 ] || { echo "ci.slice weight ${toString weight}"; exit 1; }
  ${pkgs.lib.optionalString nested "echo 'a nested gate slice (nix-builds) was taken'; exit 1"}
  mkdir -p /build/heavy && : >/build/heavy/slot1 && : >/build/heavy/all && chmod 0444 /build/heavy/slot1 /build/heavy/all
  h=${heavy}/bin/heavy
  threads=$(nproc)
  # Waits until COMMAND succeeds, for what process PID makes ready, and
  # fails if PID exits first or it takes over 30 seconds.
  ready() {
    local pid=$1 n=0; shift
    until "$@" 2>/dev/null; do
      kill -0 "$pid" 2>/dev/null || { echo "pid $pid exited before $*"; exit 1; }
      n=$(( n + 1 )); [ "$n" -lt 3000 ] || { echo "not $* after 30s"; exit 1; }
      sleep 0.01
    done
  }
  [ "$($h -- printenv HEAVY_JOBS)" = "$threads" ] || { echo "share: not $threads"; exit 1; }
  [ "$($h --all -- printenv HEAVY_SLOT HEAVY_JOBS)" = "all
$threads" ] || { echo "--all: not every slot and $threads threads"; exit 1; }
  [ "$(CARGO_BUILD_JOBS=3 $h -- printenv CARGO_BUILD_JOBS)" = 3 ] || { echo "overrode CARGO_BUILD_JOBS"; exit 1; }
  rc=0; $h -- sh -c 'exit 7' || rc=$?
  [ "$rc" = 7 ] || { echo "exit status $rc, not 7"; exit 1; }
  # Inside the slot, a nested call runs at once; a second holder waits, and gives up when told to.
  $h -- sh -c "touch held; while [ ! -e done ]; do sleep 0.1; done" &
  ready $! test -e held
  rc=0; $h --wait 1 -- true 2>err || rc=$?
  [ "$rc" = 75 ] || { echo "a second holder got in ($rc)"; exit 1; }
  HEAVY_SLOT=1 $h --wait 1 -- true || { echo "a nested call waited"; exit 1; }
  $h status | grep -q '^slot1: busy' || { echo "status: $($h status)"; exit 1; }
  touch done; wait
  $h status | grep -q '^slot1: free' || { echo "status: $($h status)"; exit 1; }

  # The caller's stdin reaches the command: a pipe, a file, a terminal, and nested.
  rc=0; printf 'exit 7\n' | $h -- sh || rc=$?
  [ "$rc" = 7 ] || { echo "piped stdin: $rc, not 7"; exit 1; }
  echo 'exit 8' >script; rc=0; $h -- sh <script || rc=$?
  [ "$rc" = 8 ] || { echo "redirected stdin: $rc, not 8"; exit 1; }
  rc=0; printf 'exit 9\n' | HEAVY_SLOT=1 $h -- sh || rc=$?
  [ "$rc" = 9 ] || { echo "nested stdin: $rc, not 9"; exit 1; }
  rc=0; printf '6\n' | ${pkgs.util-linux}/bin/script -qec "$h -- sh -c 'test -t 0 || exit 1; read n; exit \$n'" /dev/null >/dev/null || rc=$?
  [ "$rc" = 6 ] || { echo "terminal stdin: $rc, not 6"; exit 1; }

  # Cancelled, heavy stops the whole workload, a shell's worker too,
  # and holds the slot until the last of it has gone. (Background
  # jobs here ignore INT: env gives it back.) Each worker names
  # itself once its traps are set, so the signal never comes first.
  for sig in TERM INT HUP; do
    rm -f worker stopped
    env --default-signal=INT $h -- sh -c 'sh -c "trap \"sleep 1; touch stopped; exit\" TERM; echo \$\$ >worker; while :; do sleep 0.1; done" & wait' &
    hp=$!
    ready $hp test -s worker
    kill -$sig $hp; wait $hp || true
    [ -e stopped ] || { echo "$sig: the slot went before the worker stopped"; exit 1; }
    ! kill -0 "$(cat worker)" 2>/dev/null || { echo "$sig: the worker outlived heavy"; exit 1; }
    $h status | grep -q '^slot1: free' || { echo "$sig: status: $($h status)"; exit 1; }
  done
  # A command that ignores the signal, or hangs in its handler with a
  # worker that ignores everything, is killed GRACE (3) seconds after
  # the signal, worker too, and only then does the slot go.
  for sig in TERM INT HUP; do
    for cmd in \
      'trap "" TERM INT HUP; echo $$ >worker; while :; do sleep 0.1; done' \
      'trap "sleep 60" TERM INT HUP; sh -c "trap \"\" TERM INT HUP; echo \$\$ >worker; while :; do sleep 0.1; done" & wait'; do
      rm -f worker
      env --default-signal=INT $h -- sh -c "$cmd" & hp=$!
      ready $hp test -s worker
      s=$(date +%s%N); kill -$sig $hp; wait $hp || true; e=$(date +%s%N)
      ms=$(( (e - s) / 1000000 ))
      [ "$ms" -ge 2900 ] && [ "$ms" -lt 6000 ] || { echo "$sig, stubborn command: heavy took ''${ms}ms, not about 3s"; exit 1; }
      ! kill -0 "$(cat worker)" 2>/dev/null || { echo "$sig: a stubborn command outlived heavy"; exit 1; }
      $h status | grep -q '^slot1: free' || { echo "$sig, stubborn: status: $($h status)"; exit 1; }
    done
  done
  # Cancelled as it starts, nothing of the command runs on. The
  # signal goes to heavy itself: sent before the fork has become
  # heavy, it reaches only the test's shell, which in the sandbox, under
  # load, can lose it.
  for i in $(seq 10); do
    $h -- sh -c 'sleep 1; touch late' & hp=$!
    ready $hp grep -qF "$h" /proc/$hp/cmdline
    kill -TERM $hp; wait $hp || true
    sleep 1.2
    [ ! -e late ] || { echo "the command outlived a heavy cancelled at its start"; exit 1; }
  done 2>/dev/null

  # A bounded wait ends on time, and never starts the command after,
  # once the slot is free. The slot is held until the waits are done,
  # not for a time: on a loaded runner, starting heavy twice can take
  # longer than any margin.
  rm -f ran held done
  $h -- sh -c 'touch held; while [ ! -e done ]; do sleep 0.1; done' & hp=$!
  ready $hp test -e held
  s=$(date +%s%N); rc=0; $h --wait 1 -- touch ran 2>err || rc=$?; e=$(date +%s%N)
  [ "$rc" = 75 ] || { echo "--wait 1 with the slot held: $rc, not 75"; exit 1; }
  [ $(( (e - s) / 1000000 )) -lt 1800 ] || { echo "--wait 1 took $(( (e - s) / 1000000 ))ms"; exit 1; }
  rc=0; $h --wait 0 -- touch ran 2>err || rc=$?
  [ "$rc" = 75 ] || { echo "--wait 0 with the slot held: $rc, not 75"; exit 1; }
  touch done; wait $hp
  sleep 0.5
  [ ! -e ran ] || { echo "a bounded wait ran the command once the slot was free"; exit 1; }
  $h -- touch ran 2>err
  [ -e ran ] || { echo "an unbounded wait didn't get the slot"; exit 1; }
  wait
  touch $out
''
