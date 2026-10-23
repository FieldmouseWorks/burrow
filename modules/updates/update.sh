# HOST-update: keep the machine on the newest of everything it runs, unattended.
#
#   HOST-update            one run; the timer does this every 15 minutes
#   HOST-update --force    run the whole loop even if nothing upstream moved
#   HOST-update status     what the last run did, and what's queued
#
# One run: take origin/main, bump every flake input and Claude Code, build,
# run the VM tests, put it live with HOST-switch, check the box is still
# healthy, HOST-confirm, push to main. Anything that breaks goes back by itself
# and is handed to a Claude job to fix; the owner hears about it only if that
# doesn't work. It also deploys main itself, so a merged PR goes live the same
# way.
#
# It works in its own clone (WORK), never in the operator's checkout, and holds
# off while a switch is pending or while the box runs changes that aren't on
# main yet. (Everything in capitals comes from ../updates.nix.)

[ "$(id -u)" = 0 ] || exec sudo "$0" "$@"

state=$DIR
work=$WORK
operator=$FLAKE
origin=$ORIGIN
claude_releases=https://downloads.claude.ai/claude-code-releases
manifest=$MANIFEST
read -r -a checks <<<"${BURROW_UPDATE_CHECKS:-$CHECKS}"
force=

as_user() { runuser -u "$BURROW_USER" -- env HOME="$HOME_DIR" XDG_RUNTIME_DIR="/run/user/$USER_UID" "$@"; }
git_() { as_user git -C "$work" "$@"; }

note() {
  # $1 = ok|held|failed|current, $2 = what happened
  echo "$2"
  jq -n --arg r "$1" --arg m "$2" --arg t "$(date -Is)" \
    --arg run "$(readlink -f /run/current-system)" '{time: $t, result: $r, message: $m, running: $run}' \
    >"$state/status.json"
}

case "${1:-}" in
  status)
    jq -r '"last run \(.time): \(.result): \(.message)"' "$state/status.json" 2>/dev/null || echo "no run yet"
    [ ! -e "$state/held-since" ] || echo "held since $(date -d "@$(cat "$state/held-since")" -Is)"
    for f in "$state"/jobs/*; do
      [ -e "$f" ] && echo "fix job for $(basename "$f"): $(cat "$f")"
    done
    [ -z "$BOOT" ] || "$BOOT" status
    systemctl list-timers "$UPDATE.timer" ${BOOT:+"$BOOT-trial.timer"} --no-pager | sed -n '2,3p'
    exit 0
    ;;
  --force) force=1 ;;
  "") ;;
  *) echo "usage: $UPDATE [--force | status]" >&2; exit 2 ;;
esac

mkdir -p "$state/jobs" "$state/logs"
exec 9>"/run/$UPDATE.lock"
flock -n 9 || { echo "another $UPDATE is running"; exit 0; }

# Hand a problem to a Claude job in the machine's own project, once per problem while
# one is still working on it (or did in the last 6 hours).
dispatch() {
  local prompt=$2 f="$state/jobs/$1" id
  if [ -s "$f" ] && [ $(($(date +%s) - $(stat -c %Y "$f"))) -lt 21600 ]; then
    return 0
  fi
  if id=$(as_user proj run "$PROJECT" "$prompt"); then
    echo "$id" >"$f"
    echo "handed to Claude: proj log $id"
  fi
}
settled() { rm -f "$state/jobs/$1" "$state/since-$1"; "$NOTIFY" close "$1" "${2:-Cleared by a later run.}"; }
# Seconds since a problem was first seen (and stayed unsettled).
age() {
  [ -e "$state/since-$1" ] || date +%s >"$state/since-$1"
  echo $(($(date +%s) - $(cat "$state/since-$1")))
}

worktree_rules="Work in your own worktree (git -C $operator worktree add $SCRATCH/fix-\$(date +%s) origin/main), never in $operator itself: that checkout is the operator's. When the toplevel builds and \`nix build -L ${checks[*]/#/.#checks.$SYSTEM.}\` passes there, commit and push to main; $UPDATE deploys main on its next run, with the dead man's switch. If the right answer is to wait for upstream, say so and change nothing. If it needs $OWNER, say exactly what."

# Never fight a human: a pending switch is someone mid-change.
if [ -e "$SWITCH_STATE/pending" ]; then
  note held "a $SWITCH is pending; trying again next run"
  exit 0
fi

# 1. A clean copy of main.
if [ ! -d "$work/.git" ]; then
  install -d -o "$BURROW_USER" -g "$BURROW_GROUP" "$(dirname "$work")"
  as_user git clone -q "$origin" "$work"
fi
git_ fetch -q origin main
git_ checkout -q -B update origin/main
git_ reset -q --hard origin/main
git_ clean -qfdx
main=$(git_ rev-parse HEAD)

build() {
  as_user nix build --no-link --print-out-paths \
    "$work#nixosConfigurations.$CONFIG.config.system.build.toplevel" 2>"$state/logs/build.log" | tail -n1
}

# Is the tree's system the one running, apart from the commit it's stamped
# with? (A build from a dirty checkout, then committed, differs only there.)
same_as_running() {
  local r=null out
  [ -z "$rev" ] || r="\"$rev\""
  out=$(as_user nix build --impure --no-link --print-out-paths --expr "
    let f = builtins.getFlake \"git+file://$work?rev=$(git_ rev-parse HEAD)\"; in
    (f.nixosConfigurations.$CONFIG.extendModules {
      modules = [ ({ lib, ... }: { system.configurationRevision = lib.mkForce $r; }) ];
    }).config.system.build.toplevel" 2>>"$state/logs/build.log" | tail -n1) || return 1
  [ "$out" = "$current" ]
}

# 2. Does the box run main, or something main has since moved past? Then it's ours
# to update. If it runs changes that aren't on main, someone is mid-work: hold.
current=$(readlink -f /run/current-system)
rev=$(/run/current-system/sw/bin/nixos-version --configuration-revision 2>/dev/null || true)
[[ $rev =~ ^[0-9a-f]{40}(-dirty)?$ ]] || rev=
in_sync=
if [ -n "$rev" ] && [[ $rev != *dirty* ]] && git_ merge-base --is-ancestor "$rev" "$main" 2>/dev/null; then
  in_sync=1
elif [ "$current" = "$(cat "$state/deployed" 2>/dev/null)" ]; then
  in_sync=1
elif same_as_running; then
  in_sync=1
fi
if [ -z "$in_sync" ]; then
  [ -e "$state/held-since" ] || date +%s >"$state/held-since"
  if [ $(($(date +%s) - $(cat "$state/held-since"))) -gt 21600 ]; then
    "$NOTIFY" open not-on-main "$HOST runs changes that aren't on main" \
      "For over 6 hours $HOST has been running a system that isn't built from $origin main ($current, revision ${rev:-unknown}), so automatic updates are on hold. Ask the operator to commit and push what it switched to (or to switch back to main)."
  fi
  note held "$HOST runs changes that aren't on main; holding (see $UPDATE status)"
  exit 0
fi
rm -f "$state/held-since"
"$NOTIFY" close not-on-main "$HOST is back on main."

# 3. What's new upstream: every flake input, and Claude Code's latest release.
inputs=$(as_user nix flake metadata --json "$work" | jq -r '.locks.nodes.root.inputs | keys[]')

update_one() {
  if [ "$1" = claude-code ]; then
    [ -n "$manifest" ] || return 0
    local v
    v=$(curl -fsS --max-time 30 "$claude_releases/latest") || return 0
    [ "$v" != "$(jq -r .version "$work/$manifest")" ] || return 0
    curl -fsS --max-time 60 "$claude_releases/$v/manifest.zst.json" -o "$state/manifest.json" &&
      jq -e '.version and .platforms["linux-x64"].checksum' "$state/manifest.json" >/dev/null &&
      install -o "$BURROW_USER" -g "$BURROW_GROUP" -m 644 "$state/manifest.json" "$work/$manifest"
  else
    as_user nix flake update "$1" --flake "$work" 2>>"$state/logs/update.log"
  fi
}

# "nixpkgs cd4ea8d -> 1a2b3c4, " for each input that moved since $1 (main).
describe() {
  local base=${1:-$main} i old new
  for i in $inputs; do
    old=$(git_ show "$base:flake.lock" | jq -r --arg i "$i" '.nodes[.nodes.root.inputs[$i]].locked.rev // "" | .[0:7]')
    new=$(jq -r --arg i "$i" '.nodes[.nodes.root.inputs[$i]].locked.rev // "" | .[0:7]' "$work/flake.lock")
    [ "$old" = "$new" ] || printf '%s %s -> %s, ' "$i" "$old" "$new"
  done
  [ -n "$manifest" ] || return 0
  old=$(git_ show "$base:$manifest" | jq -r .version)
  new=$(jq -r .version "$work/$manifest")
  [ "$old" = "$new" ] || printf 'claude-code %s -> %s, ' "$old" "$new"
}

commit() {
  git_ diff --quiet && return 1
  local what
  what=$(describe HEAD)
  git_ commit -qam "Update ${what%, }" -m "Pushed by $UPDATE after the build, the VM tests and the health check passed."
}

# A tree that failed to build or test in the last day isn't tried again (a
# failing VM test would otherwise rerun every 15 minutes); --force retries.
tree() { git_ rev-parse 'HEAD^{tree}'; }
failed_before() {
  [ -z "$force" ] || return 1
  awk -v t="$1:$(tree)" -v since=$(($(date +%s) - 86400)) '$1 == t && $2 > since { found = 1 } END { exit !found }' \
    "$state/failed-trees" 2>/dev/null
}
remember_failed() { echo "$1:$(tree) $(date +%s)" >>"$state/failed-trees"; }

: >"$state/logs/update.log"
for i in $inputs claude-code; do update_one "$i" || true; done
changed=
commit && changed=1

if [ -z "$changed" ] && [ -z "$force" ] && { [ "$rev" = "$main" ] || [ "$(cat "$state/deployed-main" 2>/dev/null)" = "$main" ] || same_as_running; }; then
  note current "up to date"
  exit 0
fi

# 4. Build. If everything at once doesn't, take what does, one input at a time,
# so one broken input never holds back another's security fix.
failed=()
if failed_before build || ! new=$(build) || [ -z "$new" ]; then
  failed_before build || { cp "$state/logs/build.log" "$state/logs/build-all.log"; remember_failed build; }
  git_ reset -q --hard "$main"
  for i in $inputs claude-code; do
    update_one "$i" || true
    commit || continue
    if ! failed_before build && new=$(build) && [ -n "$new" ]; then
      echo "$i: builds"
    else
      failed_before build || { cp "$state/logs/build.log" "$state/logs/build-$i.log"; remember_failed build; }
      git_ reset -q --hard HEAD~1
      failed+=("$i")
    fi
  done
  new=$(build) || {
    note failed "main itself doesn't build"
    dispatch build-main "$UPDATE: $origin main ($main) doesn't build; the log is $state/logs/build.log. Fix it. $worktree_rules"
    "$NOTIFY" open build-main "main doesn't build" "$UPDATE can't build $origin main ($main). A Claude job is on it; this closes when main builds again."
    exit 1
  }
fi
if [ ${#failed[@]} -gt 0 ]; then
  dispatch build-inputs "$UPDATE: these updates don't build with $HOST's flake: ${failed[*]}. The logs are $state/logs/build-<input>.log. Everything else went through without them. Make them build. $worktree_rules"
else
  settled build-inputs
fi
settled build-main "main builds again."

if [ "$new" = "$current" ]; then
  note current "nothing new builds; up to date${failed[*]:+ (held back: ${failed[*]})}"
  exit 0
fi

# 5. The VM tests, under KVM, on exactly this tree.
for c in "${checks[@]}"; do
  if failed_before "$c" || ! as_user nix build --no-link -L "$work#checks.$SYSTEM.$c" >"$state/logs/check-$c.log" 2>&1; then
    failed_before "$c" || remember_failed "$c"
    note failed "VM test $c fails on $(git_ rev-parse --short HEAD) ($(describe))"
    dispatch "check-$c" "$UPDATE: the VM test $c fails with these updates on top of main: $(describe). The log is $state/logs/check-$c.log; the tree is $work at $(git_ rev-parse HEAD) (don't edit there, it's reset every run). Find out whether the updates or main broke it, and fix it. $worktree_rules"
    if [ "$(age "check-$c")" -gt 86400 ]; then
      "$NOTIFY" open "check-$c" "Updates stuck: VM test $c fails" \
        "For a day now $UPDATE can't get the VM test $c to pass with the latest updates ($(describe)), so they aren't deployed. Claude jobs have tried; see \`proj status\` on $HOST."
    fi
    exit 1
  fi
  settled "check-$c" "The VM test $c passes again."
done

# 6. Live, with the dead man's switch; healthy, or straight back.
"$HEALTH" snapshot "$state/health-before"
claude_before=$(as_user claude --version 2>/dev/null || true)
rc=0
BURROW_CONFIRM_WITHIN=$WITHIN "$SWITCH" --toplevel "$new" >"$state/logs/switch.log" 2>&1 || rc=$?
if [ "$rc" = 3 ]; then
  # Refused before anything went live: an older nixpkgs than the running one.
  note failed "the switch refused it as a downgrade; nothing changed"
  dispatch switch "$UPDATE: $SWITCH refused $new ($(describe)) as built from an older nixpkgs than the running system, so nothing went live. The output is $state/logs/switch.log. Find out why the new system looks older (each reads nixpkgs' date from its /etc/burrow/inputs.json) and fix it. $worktree_rules"
  exit 1
fi
if [ "$rc" != 0 ]; then
  "$SWITCH" --abort >>"$state/logs/switch.log" 2>&1 || true
  note failed "activation failed; reverted"
  dispatch activation "$UPDATE: switching to $new ($(describe)) failed during activation and was reverted. The output is $state/logs/switch.log; journalctl has the rest. Fix it. $worktree_rules"
  exit 1
fi
if ! bad=$("$HEALTH" compare "$state/health-before" 180); then
  "$SWITCH" --abort >>"$state/logs/switch.log" 2>&1 || true
  note failed "unhealthy after the switch ($bad); reverted"
  dispatch health "$UPDATE switched $HOST to $new ($(describe)) and these regressed: $bad. It reverted right away. Find out why (journalctl, the unit logs, a diff of the two systems: nix store diff-closures $current $new) and fix it. $worktree_rules"
  "$NOTIFY" open health "An update broke something and was rolled back" \
    "$UPDATE switched to $(describe) and then: $bad. $HOST went straight back to the previous system. A Claude job is on it; this closes when an update goes through."
  exit 1
fi

# Claude Code itself: if it changed, make sure it still answers. A failure here
# is noted, not reverted: it can be Anthropic's side as easily as ours.
if [ "$(as_user claude --version 2>/dev/null || true)" != "$claude_before" ]; then
  if ! (cd "$HOME_DIR" && as_user timeout 120 claude -p "Reply with the single word OK." --max-turns 1) 2>&1 | grep -q OK; then
    echo "warning: the new Claude Code didn't answer a one-line prompt" | systemd-cat -t "$UPDATE" -p warning
  fi
fi

"$CONFIRM" >>"$state/logs/switch.log" 2>&1 || {
  note failed "health was fine but $CONFIRM failed; it reverts by itself"
  exit 1
}
settled switch "An update went through."
settled activation "An update went through."
settled health "An update went through."

# 7. Push, and bring the operator's checkout along when it isn't mid-work.
echo "$new" >"$state/deployed"
if git_ push -q origin HEAD:main; then
  git_ rev-parse HEAD >"$state/deployed-main"
else
  echo "push to main was rejected (main moved); the next run rebases"
fi
if [ "$(as_user git -C "$operator" branch --show-current 2>/dev/null)" = main ] &&
  as_user git -C "$operator" diff --quiet HEAD 2>/dev/null; then
  as_user git -C "$operator" pull -q --ff-only origin main || true
fi

note ok "live and confirmed: ${changed:+$(describe)}${changed:-main $(git_ rev-parse --short HEAD)}${failed[*]:+ (held back: ${failed[*]})}"
[ -z "$BOOT" ] || ! "$BOOT" status | grep -q 'trial queued: /' ||
  echo "the new kernel boots once at the next trial window ($BOOT trial)"
exit 0
