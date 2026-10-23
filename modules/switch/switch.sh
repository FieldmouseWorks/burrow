# HOST-switch: change the running system with a dead man's switch. (The names
# come from ../switch.nix: SWITCH, CONFIRM, REVERT, STATE, FLAKE, CONFIG and
# WITHIN are set above this line.)
#
#   HOST-switch [FLAKE_DIR]        build FLAKE_DIR#CONFIG (default: the machine's flake)
#   HOST-switch --toplevel PATH    use an already-built system instead
#   HOST-switch --status           show what's pending
#
# The new system goes live at once but is NOT made the boot default. Unless
# HOST-confirm runs within BURROW_CONFIRM_WITHIN (default 10min), the previous
# system is switched back in by itself. If the box hangs outright, a hardware
# watchdog (if it has one) reboots it into the last confirmed generation. So a
# change that cuts off SSH, Tailscale or Claude itself undoes itself.

# Everything here is root's business; sudo is passwordless for the user.
[ "$(id -u)" = 0 ] || exec sudo env BURROW_CONFIRM_WITHIN="${BURROW_CONFIRM_WITHIN:-$WITHIN}" "$0" "$@"

state=$STATE
within=${BURROW_CONFIRM_WITHIN:-$WITHIN}

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
  rc=0
  "$prev/bin/switch-to-configuration" test || rc=$?
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
  --property=Type=oneshot --setenv=PATH="$PATH" \
  "$(readlink -f "$0")" --revert-to "$prev"

# Activate in its own unit, so it finishes even if it restarts whatever called it.
if ! systemd-run --unit="$SWITCH-activate" --collect --wait --pipe --quiet \
  "$new/bin/switch-to-configuration" test; then
  echo "activation reported errors; it will revert in $within unless you run $CONFIRM" >&2
  exit 1
fi
echo "live: $new"
echo "run $CONFIRM within $within to keep it (and make it the boot default)"
