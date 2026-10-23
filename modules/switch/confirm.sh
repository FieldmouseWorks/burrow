# HOST-confirm: keep the system HOST-switch put live, and boot into it from now
# on. (STATE and REVERT come from ../switch.nix.)
[ "$(id -u)" = 0 ] || exec sudo "$0" "$@"
state=$STATE

# Hold off any switch until this is done (see switch.sh).
mkdir -p $state
exec 9>$state/lock
flock 9

if [ "$(systemctl show -p ActiveState --value "$REVERT.service")" = activating ]; then
  echo "too late: the revert is already running" >&2
  exit 1
fi
[ -e $state/pending ] || { echo "nothing pending"; exit 0; }
new=$(cat $state/pending)
if [ "$(readlink -f /run/current-system)" != "$new" ]; then
  echo "running system isn't the pending one (did it revert?); not confirming" >&2
  exit 1
fi
# Make it the boot default first, and only then disarm the revert: if this
# fails halfway, the change still reverts on schedule.
# Through the daemon, as the user's builds do; root opening the store directly can
# trip over how /nix/store is mounted.
if ! env NIX_REMOTE=daemon nix-env -p /nix/var/nix/profiles/system --set "$new" ||
  ! "$new/bin/switch-to-configuration" boot; then
  echo "couldn't make it the boot default; still pending, and it reverts on schedule" >&2
  exit 1
fi
systemctl stop "$REVERT.timer" 2>/dev/null || true
# The timer may have fired just before we stopped it; let that revert land
# before looking at which system is live.
while [ "$(systemctl show -p ActiveState --value "$REVERT.service")" = activating ]; do sleep 1; done
if [ "$(readlink -f /run/current-system)" != "$new" ]; then
  # The revert fired while we were confirming. Boot what's actually running.
  cur=$(readlink -f /run/current-system)
  env NIX_REMOTE=daemon nix-env -p /nix/var/nix/profiles/system --set "$cur"
  "$cur/bin/switch-to-configuration" boot
  rm -f $state/pending $state/prev
  echo "too late: it reverted while confirming; $cur stays the boot default" >&2
  exit 1
fi
rm -f $state/pending $state/prev
echo "confirmed: $new is the boot default"
