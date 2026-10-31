# HOST-switch: change the running system with a dead man's switch. (The names
# come from ../switch.nix: SWITCH, CONFIRM, REVERT, STATE, FLAKE, CONFIG and
# WITHIN are set above this line.)
#
#   HOST-switch [FLAKE_DIR]        build FLAKE_DIR#CONFIG (default: the machine's flake)
#   HOST-switch --toplevel PATH    use an already-built system instead
#   HOST-switch --status           show what's pending
#   HOST-switch --abort            revert a pending switch now instead of waiting
#
# It won't put a system live whose nixpkgs is older than the running one's (that
# would quietly undo security fixes); --allow-downgrade, anywhere on the line,
# says you mean it, and without it the refusal exits 3. (It knows nixpkgs' date
# from /etc/burrow/inputs.json, which burrow.switch.flakeInputs writes.)
#
# The new system goes live at once but is NOT made the boot default. Unless
# HOST-confirm runs within BURROW_CONFIRM_WITHIN (default 10min), the previous
# system is switched back in by itself. If the box hangs outright, a hardware
# watchdog (if it has one) reboots it into the last confirmed generation. So a
# change that cuts off SSH, Tailscale or Claude itself undoes itself. An
# activation that hangs is killed three quarters of the way through the window
# and reverted at once, so the old system is back before the window ends.

# Everything here is root's business; sudo is passwordless for the user.
[ "$(id -u)" = 0 ] || exec sudo env BURROW_CONFIRM_WITHIN="${BURROW_CONFIRM_WITHIN:-$WITHIN}" "$0" "$@"

state=$STATE
within=${BURROW_CONFIRM_WITHIN:-$WITHIN}

downgrade=
args=()
for a in "$@"; do
  if [ "$a" = --allow-downgrade ]; then downgrade=1; else args+=("$a"); fi
done
set -- "${args[@]}"

# systemd-oomd (burrow.ops.oomd) never picks the switch's own units: a killed
# revert would leave the unconfirmed system live with nothing left to undo it,
# and a killed activation leaves it half switched.
keep=(--property=ManagedOOMPreference=omit)

# The revert is a oneshot: "activating" for as long as it runs.
reverting() {
  case $(systemctl show -p ActiveState --value "$REVERT.service") in
    activating | active | deactivating) return 0 ;;
    *) return 1 ;;
  esac
}

# Run by the revert unit when the switch wasn't confirmed in time.
if [ "${1:-}" = --revert-to ]; then
  prev=$2
  # switch-to-configuration holds this lock while it activates, and refuses at
  # once (exit 11) while someone else holds it: the switch being undone, when
  # activating outlasts the window, or a confirm writing the boot default. A
  # refused activation changed nothing, and /run/current-system may not have
  # moved yet, so wait that one out and then put prev back.
  until
    flock /run/nixos/switch-to-configuration.lock true
    rc=0
    "$prev/bin/switch-to-configuration" test || rc=$?
    [ "$rc" != 11 ]
  do :; done
  # A unit failing to restart still counts as switched back (exit 4); what
  # matters is which system is live now.
  if [ "$(readlink -f /run/current-system)" = "$prev" ]; then
    rm -f $state/pending $state/revert-failed
    echo "not confirmed, reverted to $prev (activation exit $rc)" | systemd-cat -t "$SWITCH" -p warning
  else
    echo "$prev" >$state/revert-failed
    echo "REVERT FAILED (exit $rc): still running the unconfirmed $(readlink -f /run/current-system)" |
      systemd-cat -t "$SWITCH" -p err
    exit 1
  fi
  exit 0
fi

# Run as the activation unit stops. Past its deadline, the revert starts now:
# waiting for the timer would land it after the window. If the timer is no
# longer armed, a confirm or an abort has already settled this switch.
if [ "${1:-}" = --activation-stopped ]; then
  if [ "${SERVICE_RESULT:-}" = timeout ] &&
    [ "$(systemctl show -p SubState --value "$REVERT.timer")" = waiting ]; then
    echo "activation killed: it hung past its deadline, reverting now" | systemd-cat -t "$SWITCH" -p warning
    # Killing it leaves its jobs running in systemd, and a stop that never ends
    # would hold up the revert's start of that unit in turn. Only these need a
    # hand: the revert itself stops a unit that's stuck starting.
    while read -r _ unit type job_state; do
      case $type:$job_state in
        stop:running | restart:running | reload:running)
          echo "killing $unit: its $type is stuck" | systemd-cat -t "$SWITCH" -p warning
          systemctl kill --signal=SIGKILL "$unit" || true
          ;;
      esac
    done < <(systemctl list-jobs --plain --no-legend)
    # In this order: with the timer gone first, nothing would hold the inactive
    # transient service loaded.
    systemctl start --no-block "$REVERT.service"
    systemctl stop "$REVERT.timer"
  fi
  exit 0
fi

if [ "${1:-}" = --status ]; then
  if [ -e $state/revert-failed ]; then
    echo "REVERT FAILED: still on the unconfirmed $(readlink -f /run/current-system);"
    echo "the last confirmed system is $(cat $state/revert-failed) (see journalctl -t $SWITCH)"
  fi
  if reverting; then
    echo "reverting to $(cat $state/prev 2>/dev/null) right now"
  elif [ -e $state/pending ]; then
    echo "pending: $(cat $state/pending)"
    echo "reverts to: $(cat $state/prev)"
    systemctl list-timers "$REVERT.timer" --no-pager | sed -n 2p
  else
    echo "nothing pending; running $(readlink -f /run/current-system)"
  fi
  exit 0
fi

if [ "${1:-}" = --abort ]; then
  mkdir -p $state
  exec 9>$state/lock
  flock 9
  [ -e $state/pending ] || { echo "nothing pending"; exit 0; }
  systemctl stop "$REVERT.timer" 2>/dev/null || true
  # A revert that already fired finishes on its own.
  while reverting; do sleep 1; done
  [ -e $state/pending ] || { echo "already reverted"; exit 0; }
  systemd-run --unit="$REVERT-now" "${keep[@]}" --collect --wait --pipe --quiet --setenv=PATH="$PATH" \
    "$(readlink -f "$0")" --revert-to "$(cat $state/prev)"
  exit
fi

if [ "${1:-}" = --toplevel ]; then
  new=$(readlink -f "$2")
else
  flake=$(readlink -f "${1:-$FLAKE}")
  # Build as the checkout's owner: git refuses repos owned by someone else.
  # runuser, not sudo: under systemd-run there are no setuid wrappers on PATH.
  owner=$(stat -c %U "$flake")
  run=(); [ "$(id -un)" = "$owner" ] || run=(runuser -u "$owner" --)
  new=$("${run[@]}" nix build --no-link --print-out-paths \
    "$flake#nixosConfigurations.$CONFIG.config.system.build.toplevel")
fi
[ -x "$new/bin/switch-to-configuration" ] || { echo "not a NixOS system: $new" >&2; exit 1; }

if [ "$new" = "$(readlink -f /run/current-system)" ]; then
  echo "already running $new"
  exit 0
fi

# nixpkgs' commit time, as built into each system (burrow.switch.flakeInputs).
stamp() { jq -r '.nixpkgs.lastModified // 0' "$1/etc/burrow/inputs.json" 2>/dev/null || echo 0; }
if [ -z "$downgrade" ] && [ "$(stamp "$new")" -lt "$(stamp /run/current-system)" ]; then
  echo "refusing: $new is built from an older nixpkgs than the running system" >&2
  echo "(that would undo security fixes; pull origin/main first, or pass --allow-downgrade)" >&2
  exit 3
fi

# One switch or confirm at a time: each owns pending and the revert timer
# until it's done. (The revert itself doesn't lock; both wait for it instead.)
mkdir -p $state
exec 9>$state/lock
flock 9

# Revert to the last *confirmed* system, even if an earlier switch is still
# pending. Read under the lock: a confirm may have just moved it.
prev=$(readlink -f /nix/var/nix/profiles/system)

# Disarm any earlier pending switch, and if its revert already fired, let that
# land rather than interrupt it halfway through activation.
systemctl stop "$REVERT.timer" 2>/dev/null || true
while reverting; do sleep 1; done
systemctl reset-failed "$REVERT.timer" "$REVERT.service" 2>/dev/null || true
rm -f $state/revert-failed
echo "$prev" >$state/prev
echo "$new" >$state/pending

# Arm the revert before touching anything.
# Transient units start with a bare PATH; hand ours (coreutils, systemd) over.
systemd-run --unit="$REVERT" --on-active="$within" --timer-property=AccuracySec=1s \
  --property=Type=oneshot "${keep[@]}" --setenv=PATH="$PATH" \
  "$(readlink -f "$0")" --revert-to "$prev"

# Activate in its own unit, so it finishes even if it restarts whatever called
# it. switch-to-configuration holds the lock the revert needs, so a hung
# activation would hold off the revert for good: it gets three quarters of the
# window, and the rest is the revert's. Killing it halfway leaves nothing behind:
# the revert picks up the units it stopped from the list switch-to-configuration
# keeps in /run/nixos, --activation-stopped clears its stuck jobs, and
# tests/switch.nix holds them to that.
# (Its "μs:" line, which says "us:" outside a UTF-8 locale.)
while read -r key value; do
  case $key in *s:) window_us=$value ;; esac
done < <(systemd-analyze timespan "$within")
rc=0
systemd-run --unit="$SWITCH-activate" "${keep[@]}" --collect --wait --pipe --quiet \
  --property=RuntimeMaxSec="$((window_us * 3 / 4))us" --property=TimeoutStopSec=5s \
  --property=ExecStopPost="$(readlink -f "$0") --activation-stopped" \
  "$new/bin/switch-to-configuration" test || rc=$?
# The timer's gone once the revert has fired or the deadline started it: the old
# system is coming back whatever the activation said.
if [ "$(systemctl show -p SubState --value "$REVERT.timer")" != waiting ]; then
  echo "not kept: activating took too long for a $within window, so it reverts to $prev" >&2
  exit 1
fi
if [ "$rc" != 0 ]; then
  echo "activation reported errors; it will revert in $within unless you run $CONFIRM" >&2
  exit 1
fi
echo "live: $new"
echo "run $CONFIRM within $within to keep it (and make it the boot default)"
