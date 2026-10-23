# PreToolUse guard for Claude Code. Implements the reversibility classes:
#
#   R0  undoable in seconds       -> allowed silently (the default)
#   R1  undoable, but only with a primitive -> take the primitive, then allow
#   R2  one-way door              -> ask the owner
#
# The R2 list that can be written as patterns lives in permissions.ask in the
# managed settings (../claude.nix); those prompt even in bypassPermissions mode.
# This hook covers what patterns can't: it snapshots before file destruction,
# and asks before rewriting a default branch upstream.
#
# Input: the hook JSON on stdin. Output: nothing (allow), or a decision JSON.

input=$(cat)
tool=$(jq -r '.tool_name // empty' <<<"$input")
[ "$tool" = "Bash" ] || exit 0
cmd=$(jq -r '.tool_input.command // empty' <<<"$input")

ask() {
  jq -n --arg r "$1" '{hookSpecificOutput: {hookEventName: "PreToolUse",
    permissionDecision: "ask", permissionDecisionReason: $r}}'
  exit 0
}

# R2: force-pushing (or deleting) a default branch rewrites shared history.
# Resolve what the push actually targets: explicit refspecs, or the current
# branch and its upstream when there are none (`git push --force` on main).
cwd=$(jq -r '.cwd // empty' <<<"$input")
if push=$(grep -oE 'git([[:space:]]+-C[[:space:]]+[^[:space:];&|]+)?[[:space:]]+push([[:space:]][^;&|]*)?' <<<"$cmd" | head -n1) && [ -n "$push" ]; then
  if [[ $push =~ -C[[:space:]]+([^[:space:]]+) ]]; then
    case ${BASH_REMATCH[1]} in /*) cwd=${BASH_REMATCH[1]} ;; *) cwd=${cwd:-.}/${BASH_REMATCH[1]} ;; esac
  fi
  read -ra words <<<"${push#*push}"
  force=0 all=0 positional=()
  for w in "${words[@]}"; do
    case $w in
      -f | --force | --force-with-lease* | --force-if-includes | -d | --delete) force=1 ;;
      --all) all=1 ;;
      # --mirror and --prune force-update and delete remote refs by design.
      --mirror | --prune) force=1 all=1 ;;
      -*) ;;
      +*) force=1 positional+=("${w#+}") ;;
      :*) force=1 positional+=("${w#:}") ;;
      *) positional+=("$w") ;;
    esac
  done
  if [ "$force" = 1 ]; then
    [ "$all" = 1 ] && ask "Push that can rewrite or delete every remote branch (R2: rewrites history other clones depend on)."
    targets=()
    if [ "${#positional[@]}" -gt 1 ]; then
      for r in "${positional[@]:1}"; do
        dest=${r##*:}
        targets+=("${dest#refs/heads/}")
      done
    else
      targets+=("$(git -C "${cwd:-.}" symbolic-ref --short -q HEAD 2>/dev/null || true)")
      up=$(git -C "${cwd:-.}" rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null) && targets+=("${up#*/}")
    fi
    for t in "${targets[@]}"; do
      case $t in
        main | master | trunk | HEAD)
          ask "Force-push or delete of '$t' (R2: rewrites history other clones depend on)." ;;
      esac
    done
  fi
fi

# R2: firmware settings that can stop the box booting. Reading them is fine;
# changing UEFI boot entries or writing EFI variables directly is not.
if grep -Eq '(^|[;&|[:space:]])efibootmgr([[:space:]][^;&|]*)?[[:space:]](-[a-zA-Z]*[cCBbAaoOnNTt]|--(create|delete|bootorder|bootnext|active|inactive|delete-bootorder|timeout|delete-timeout))' <<<"$cmd"; then
  ask "Changes UEFI boot entries (R2: a bad one can stop the box booting)."
fi
if grep -q '/sys/firmware/efi/efivars' <<<"$cmd" &&
  grep -Eq '(^|[;&|[:space:]])(rm|chattr|tee|dd|cp|mv|install|efivar)[[:space:]]|>[[:space:]]*/sys/firmware/efi/efivars' <<<"$cmd"; then
  ask "Writes EFI variables (R2: a bad one can stop the box booting)."
fi

# R2: the Hetzner Robot calls that can't be taken back. `robot` (../robot.nix)
# only offers status, rescue, reset and wake-on-LAN; a raw call that cancels
# the server or arms a reinstall (which wipes the disks on the next reset) asks.
if grep -Eq 'robot-ws\.your-server\.de' <<<"$cmd" &&
  grep -Eq 'cancellation|/boot/[^/[:space:]]+/(linux|windows|vnc|plesk|cpanel)|/order/' <<<"$cmd"; then
  ask "Hetzner Robot call that cancels, orders or reinstalls a server (R2: no undo)."
fi

# R1: anything that deletes or rewrites working files gets a ZFS snapshot of
# GUARD_SNAPSHOT_DATASETS first (burrow.undo.datasets; ../claude.nix sets it).
# Snapshots are O(1) and pruned after a few days (../undo.nix), so this turns `rm -rf`, `git clean`, `git reset --hard` and friends into instant undo
# instead of a question. Every git checkout/switch/restore/reset counts, since
# path and forced forms overwrite uncommitted work. Redirects, cp and tee count
# too (they overwrite), except redirects into /dev (2>/dev/null and friends).
if [ "${#GUARD_SNAPSHOT_DATASETS[@]}" -gt 0 ] && grep -Eq '(^|[;&|[:space:](])(rm|rmdir|shred|truncate|unlink|git([[:space:]]+-C[[:space:]]+[^[:space:]]+)?[[:space:]]+(clean|reset|checkout|switch|restore|stash|branch[[:space:]]+-D|worktree[[:space:]]+remove|filter-branch|filter-repo))([[:space:]]|$)|(^|[^0-9&>])>>?[[:space:]]*([^&>/[:space:]]|/[^d]|/d[^e])|rsync[[:space:]].*--delete|find[[:space:]].*-delete|(^|[;&|[:space:]])(mv|cp|tee|sed[[:space:]]+-i|perl[[:space:]]+-p?i)[[:space:]]' <<<"$cmd"; then
  # Collapse bursts: one round of snapshots per 5 seconds is plenty.
  mark="${XDG_RUNTIME_DIR:-/tmp}/claude-guard-last-snapshot"
  if [ ! -e "$mark" ] || [ $(($(date +%s) - $(stat -c %Y "$mark"))) -ge 5 ]; then
    stamp=$(date +%Y%m%d-%H%M%S)
    # No snapshot, no undo: then it asks instead.
    if ! err=$(zfs snapshot "${GUARD_SNAPSHOT_DATASETS[@]/%/@pre-$stamp}" 2>&1); then
      ask "Couldn't take the undo snapshot first (${err:-zfs snapshot failed}), so this would have no undo."
    fi
    touch "$mark"
  fi
fi

exit 0
