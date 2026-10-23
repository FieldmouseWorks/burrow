# HOST-boot: GRUB only ever defaults to a kernel that has already booted on
# this machine. A new kernel, initrd or kernel command line gets tried once, at
# a quiet hour, and becomes the default only after it comes up healthy. If it
# panics or hangs, the panic timer or the hardware watchdog resets the board
# and GRUB boots the last good one. (BOOT, HOST, DIR, SWITCH_STATE, NOTIFY and
# BURROW_USER come from ../switch.nix.)
#
#   HOST-boot default SYSTEM   make SYSTEM the boot default if its boot bits
#                              have booted before; otherwise keep the current
#                              default and queue a trial (HOST-confirm runs this)
#   HOST-boot trial [--force]  reboot into the queued trial once, if the box is
#                              idle (--force: don't wait for jobs and sandboxes)
#   HOST-boot check            after boot: settle the trial, remember what booted
#   HOST-boot status

[ "$(id -u)" = 0 ] || exec sudo "$0" "$@"

state=$DIR
proven=$state/boot-proven
failed=$state/boot-failed
trial=$state/boot-trial
default_file=$state/boot-default
mkdir -p $state
touch $proven $failed

# What the bootloader actually hands over: kernel, initrd and command line
# (init= aside, which is the system itself and changes with every generation).
bits() {
  printf '%s %s %s' "$(readlink -f "$1/kernel")" "$(readlink -f "$1/initrd")" "$(cat "$1/kernel-params")" |
    sha256sum | cut -d' ' -f1
}

is_proven() {
  local b
  b=$(bits "$1")
  [ "$b" = "$(bits /run/booted-system)" ] || grep -qx "$b" $proven
}

# The system GRUB boots by default now: the last one this script made the
# default, else what's running since boot.
current_default() {
  local d
  d=$(cat $default_file 2>/dev/null || true)
  if [ -n "$d" ] && [ -e "$d/bin/switch-to-configuration" ] && is_proven "$d"; then
    echo "$d"
  else
    readlink -f /run/booted-system
  fi
}

make_default() {
  "$1/bin/switch-to-configuration" boot
  echo "$1" >$default_file
}

# The GRUB title path ("submenu>entry") of the menu entry that boots SYSTEM,
# read from the generated menu so it always matches what GRUB shows.
title_of() {
  awk -v want="init=$1/init " '
    /^[[:space:]]*submenu "/ { split($0, q, "\""); stack[++depth] = q[2]; next }
    /^[[:space:]]*menuentry "/ { split($0, q, "\""); entry = q[2]; next }
    /^[[:space:]]*}/ { if (entry != "") entry = ""; else if (depth > 0) depth--; next }
    entry != "" && index($0 " ", want) {
      path = ""
      for (i = 1; i <= depth; i++) path = path stack[i] ">"
      found = path entry
    }
    END { if (found == "") exit 1; print found }
  ' /boot/grub/grub.cfg
}

editenv() {
  for b in /boot /boot-fallback; do
    [ -e "$b/grub" ] || continue
    [ -e "$b/grub/grubenv" ] || grub-editenv "$b/grub/grubenv" create
    grub-editenv "$b/grub/grubenv" "$@"
  done
}

# Agent work in flight that a reboot would cut off.
busy() {
  local n
  n=$( { systemctl --user -M "$BURROW_USER@" list-units --plain --no-legend --state=active 'proj-*.service' 2>/dev/null || true; } | wc -l)
  n=$((n + $( { systemctl list-units --plain --no-legend --state=active 'microvm@*.service' 2>/dev/null || true; } | wc -l)))
  [ "$n" -gt 0 ]
}

case "${1:-status}" in
  default)
    new=$(readlink -f "${2:?usage: $BOOT default SYSTEM}")
    if is_proven "$new"; then
      make_default "$new"
      rm -f $trial
      echo "boot default: $new"
    elif grep -qx "$(bits "$new")" $failed; then
      # Its kernel already failed to boot here once; never queue it again.
      make_default "$(current_default)"
      echo "boot default stays $(current_default): $new's kernel failed to boot before" >&2
    else
      # Regenerate the menu (so the new generation is in it) with the proven
      # default still on top, and queue the new one for a trial boot.
      d=$(current_default)
      make_default "$d"
      echo "$new" >$trial
      echo "boot default stays $d; $new boots once in the next trial ($BOOT trial)"
    fi
    ;;

  trial)
    [ -s $trial ] || { echo "no trial queued"; exit 0; }
    new=$(cat $trial)
    if is_proven "$new"; then
      make_default "$new"
      rm -f $trial
      echo "already proven; $new is the boot default"
      exit 0
    fi
    # A busy box postpones it, for two days at most.
    if [ "${2:-}" != --force ] && [ $(($(date +%s) - $(stat -c %Y $trial))) -lt 172800 ] && busy; then
      echo "jobs or sandboxes running; trial postponed"
      exit 0
    fi
    # Never with an unconfirmed switch live: a reboot would make that decision.
    if [ -e "$SWITCH_STATE/pending" ]; then
      echo "a switch is pending; trial postponed"
      exit 0
    fi
    title=$(title_of "$new") || { echo "$new is not in the GRUB menu" >&2; exit 1; }
    echo "$new" >$state/boot-trying
    editenv set "next_entry=$title"
    sync
    echo "rebooting once into: $title" | systemd-cat -t "$BOOT" -p warning
    systemctl reboot
    ;;

  check)
    booted=$(readlink -f /run/booted-system)
    trying=$(cat $state/boot-trying 2>/dev/null || true)
    rm -f $state/boot-trying
    # GRUB clears next_entry itself; make sure, so a missed clear never loops.
    editenv unset next_entry
    if [ -n "$trying" ] && [ "$booted" != "$trying" ]; then
      bits "$trying" >>$failed
      rm -f $trial
      echo "TRIAL FAILED: $trying did not come up; booted $booted instead" | systemd-cat -t "$BOOT" -p err
      "$NOTIFY" open boot-trial "A new kernel didn't boot" \
        "$HOST tried to boot $trying once and came back up on $booted instead (panic, hang or watchdog reset). That kernel won't be tried again. The default is still the last good one. Claude is looking into it."
      exit 0
    fi
    # Through multi-user with SSH up. A trial that got this far but is broken
    # goes back to the proven default (next_entry is already cleared).
    if [ -n "$trying" ] && ! systemctl -q is-active sshd.service; then
      bits "$trying" >>$failed
      rm -f $trial
      echo "TRIAL FAILED: $trying booted without sshd; rebooting into the default" | systemd-cat -t "$BOOT" -p err
      "$NOTIFY" open boot-trial "A new kernel booted broken" \
        "$HOST booted $trying once, but sshd didn't come up, so it rebooted into the last good system. That kernel won't be tried again."
      systemctl reboot
      exit 0
    fi
    # This kernel, initrd and command line work here.
    grep -qx "$(bits "$booted")" $proven || bits "$booted" >>$proven
    if [ -n "$trying" ]; then
      make_default "$booted"
      rm -f $trial
      echo "trial passed: $booted is the boot default" | systemd-cat -t "$BOOT" -p notice
      "$NOTIFY" close boot-trial "The trial boot of $booted worked."
    fi
    ;;

  status)
    echo "booted:       $(readlink -f /run/booted-system)"
    echo "running:      $(readlink -f /run/current-system)"
    echo "boot default: $(current_default)"
    if [ -s $trial ]; then echo "trial queued: $(cat $trial)"; else echo "trial queued: none"; fi
    ;;

  *)
    echo "usage: $BOOT default SYSTEM | trial [--force] | check | status" >&2
    exit 2
    ;;
esac
