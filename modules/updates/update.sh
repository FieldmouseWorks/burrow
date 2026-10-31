# HOST-update: keep the machine on the newest of everything it runs, unattended.
#
#   HOST-update            one run; burrow-events starts one for each push to
#                          main (or a watched input), and hourly
#   HOST-update --force    run the whole loop even if nothing upstream moved
#   HOST-update status     what the last run did, and what's queued
#
# One run: take origin/main, bump every flake input and Claude Code, build,
# run the VM tests, put it live with HOST-switch, check the box is still
# healthy, HOST-confirm, push to main. Anything that breaks goes back by itself
# and is handed to a Claude job to fix (acting on GitHub as the machine's own
# App, APP, when it has one); the owner hears about it only if that doesn't
# work. It also deploys main itself, so a merged PR goes live the same
# way.
#
# It works in its own clone (WORK), never in the operator's checkout, and holds
# off while a switch is pending or while the box runs changes that aren't on
# main yet. In a repo whose PRs land through its merge train, what it would push
# goes up as a PR instead, and it rides that PR until it lands (propose.sh).
# (Everything in capitals comes from ../updates.nix.)

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
    systemctl list-timers burrow-events-catch-up.timer ${BOOT:+"$BOOT-trial.timer"} --no-pager | sed -n '2,3p'
    exit 0
    ;;
  --force) force=1 ;;
  "") ;;
  *) echo "usage: $UPDATE [--force | status]" >&2; exit 2 ;;
esac

mkdir -p "$state/jobs" "$state/logs"
exec 9>"/run/$UPDATE.lock"
flock -n 9 || { echo "another $UPDATE is running"; exit 0; }

# dispatch, which hands a problem to a Claude job, is ./dispatch.sh.
settled() { rm -f "$state/jobs/$1" "$state/since-$1"; "$NOTIFY" close "$1" "${2:-Cleared by a later run.}"; }
# Seconds since a problem was first seen (and stayed unsettled).
age() {
  [ -e "$state/since-$1" ] || date +%s >"$state/since-$1"
  echo $(($(date +%s) - $(cat "$state/since-$1")))
}

if [ -n "$TRAIN" ]; then
  to_main="commit, push it to a branch of your own and open a PR against main, which the steward settles and the merge train lands (nothing else may move main)"
else
  to_main="commit and push to main"
fi
worktree_rules="Work in your own worktree (git -C $operator worktree add $SCRATCH/fix-\$(date +%s) origin/main), never in $operator itself: that checkout is the operator's. When the toplevel builds and \`nix build -L ${checks[*]/#/.#checks.$SYSTEM.}\` passes there, $to_main; $UPDATE deploys main on its next run, with the dead man's switch. If the right answer is to wait for upstream, say so and change nothing. If it needs $OWNER, say exactly what."

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

# Its PR open, in a train repo: main with that on top, and nothing fresh.
riding=
url=
if [ -n "$TRAIN" ]; then
  pr=$(update_pr) || { note held "GitHub didn't say whether its PR is open; trying again next run"; exit 0; }
  if [ -n "$pr" ]; then
    why=$(ride "$pr") || { note held "$why"; exit 0; }
    riding=${pr%% *}
  fi
fi

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
  git_ commit -qam "deps: update ${what%, }" -m "Pushed by $UPDATE after the build, the VM tests and the health check passed."
}

# A tree that failed to build in the last day isn't tried again (nor, below, a
# VM test it already passed or failed); --force retries.
tree() { git_ rev-parse 'HEAD^{tree}'; }
failed_before() {
  [ -z "$force" ] || return 1
  awk -v t="$1:$(tree)" -v since=$(($(date +%s) - 86400)) '$1 == t && $2 > since { found = 1 } END { exit !found }' \
    "$state/failed-trees" 2>/dev/null
}
remember_failed() { echo "$1:$(tree) $(date +%s)" >>"$state/failed-trees"; }

: >"$state/logs/update.log"
changed=
if [ -n "$riding" ]; then
  [ "$(git_ rev-parse HEAD)" = "$main" ] || changed=1
else
  for i in $inputs claude-code; do update_one "$i" || true; done
  commit && changed=1
fi

if [ -z "$changed" ] && [ -z "$force" ] && { [ "$rev" = "$main" ] || [ "$(cat "$state/deployed-main" 2>/dev/null)" = "$main" ] || same_as_running; }; then
  note current "up to date"
  exit 0
fi

# 4. Build. If everything at once doesn't, take what does, one input at a time,
# so one broken input never holds back another's security fix.
failed=()
if [ -n "$riding" ] && { failed_before build || ! new=$(build) || [ -z "$new" ]; }; then
  failed_before build || remember_failed build
  note failed "main with its PR #$riding on top doesn't build"
  dispatch build-riding "$UPDATE: $origin main ($main) with $UPDATE's own PR #$riding merged on top doesn't build; the log is $state/logs/build.log. Fix it on that PR's branch, in your own worktree, so the train can land it; $UPDATE holds off until it has. Never push to main."
  exit 1
fi
settled build-riding "main with its PR on top builds."
if [ -z "$riding" ] && { failed_before build || ! new=$(build) || [ -z "$new" ]; }; then
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

live=
if [ "$new" = "$current" ]; then
  # Running already, and not on main: what a run before put live (its PR
  # didn't open, or closed, or the train took it while that run was testing a
  # newer one), or a bump that changes only what the checks use. It goes up as
  # a PR with what any other bump has: the switch and the health check it
  # passed when it went live, and the VM tests below, on this tree (which isn't
  # the one that went live once main moved under it or the checks' inputs did).
  case $(already_live "$new") in
    up) live=1 ;;
    hold)
      note current "$HOST runs what $(describe) builds, but $UPDATE didn't put it live, so it never had the health check; holding it until main has it"
      exit 0
      ;;
    *)
      note current "nothing new builds; up to date${failed[*]:+ (held back: ${failed[*]})}"
      exit 0
      ;;
  esac
fi

# 5. The VM tests, under KVM, on exactly this tree.
#
# A test that fails is run once more on the same tree. If it passes then, it's
# flaky: the update goes ahead, and a Claude job is told to make the test
# reliable (the owner hears about it until it has passed first time three runs
# in a row). If it fails again, it's run on main as it was before the updates.
# Passing there means the updates broke it, and they wait for a fix. Failing
# there too means main's test is broken, which isn't the updates' fault: they
# still wait, unless they take away a serious known vulnerability from what runs
# now (CVSS 7+, per vulnix). Those go live without the test, under the same dead
# man's switch and health check as everything else.

# VM test $1 on commit $2, logged to $3.
run_check() {
  as_user nix build --no-link -L "git+file://$work?rev=$2#checks.$SYSTEM.$1" >"$3" 2>&1
}
# What VM test $1 did on commit $2's tree in the last day: pass, fail or nothing.
result_of() {
  [ -z "$force" ] && [ -e "$state/check-results" ] || return 0
  awk -v k="$1:$(git_ rev-parse "$2^{tree}")" -v since=$(($(date +%s) - 86400)) \
    '$1 == k && $3 > since { r = $2 } END { print r }' "$state/check-results" 2>/dev/null
}
record() { echo "$1:$(git_ rev-parse "$2^{tree}") $3 $(date +%s)" >>"$state/check-results"; }
# Serious known vulnerabilities in a system's closure, one "CVE package" per
# line, sorted. Fails if vulnix fails or says something that isn't its report:
# a scan that didn't happen must never read as "fixed everything".
vulns() {
  local out list
  out=$(vulnix --closure --json "$1" 2>/dev/null) || true
  [ -n "$out" ] || return 1
  list=$(jq -r 'if type != "array" then error("not a vulnix report") else .[] end
    | . as $p | .affected_by[] as $c | select((($p.cvssv3_basescore[$c]) // 0) >= 7)
    | "\($c) \($p.pname)"' <<<"$out" 2>/dev/null) || return 1
  sort -u <<<"$list"
}

head=$(git_ rev-parse HEAD)
untested=()
for c in "${checks[@]}"; do
  log=$state/logs/check-$c.log
  r=$(result_of "$c" "$head")
  if [ -z "$r" ]; then
    if run_check "$c" "$head" "$log"; then
      r=pass
      n=$(($(cat "$state/clean-$c" 2>/dev/null || echo 0) + 1))
      echo "$n" >"$state/clean-$c"
      [ "$n" -lt 3 ] || settled "flaky-$c" "The VM test $c has passed first time three runs in a row."
    elif run_check "$c" "$head" "$log.again"; then
      r=pass
      mv "$log" "$state/logs/flaky-$c.log"
      mv "$log.again" "$log"
      echo 0 >"$state/clean-$c"
      dispatch "flaky-$c" "$UPDATE: the VM test $c is flaky: on $head it failed once (log: $state/logs/flaky-$c.log) and passed when run again ($log). A flaky test holds back security updates, so find the cause and fix it, in the test or in what it tests; a retry or a longer timeout isn't a fix unless the cause really is time. $worktree_rules"
      "$NOTIFY" open "flaky-$c" "VM test $c is flaky" \
        "The VM test $c failed and then passed on the same tree, so updates went ahead. A Claude job is finding the cause; this closes once the test passes first time three runs in a row."
    else
      r=fail
      mv "$log.again" "$log"
    fi
    record "$c" "$head" "$r"
  fi
  if [ "$r" = pass ]; then
    settled "check-$c" "The VM test $c passes again."
    continue
  fi

  # It fails every time. The updates' fault, or main's?
  if [ "$head" = "$main" ]; then
    base=fail
  else
    base=$(result_of "$c" "$main")
    if [ -z "$base" ]; then
      base=fail
      run_check "$c" "$main" "$state/logs/check-$c-main.log" && base=pass
      record "$c" "$main" "$base"
    fi
  fi
  if [ "$base" = fail ]; then
    untested+=("$c")
    dispatch "check-$c" "$UPDATE: the VM test $c fails on $origin main ($main) itself, not only with the updates. The log is $log; the tree is $work (don't edit there, it's reset every run). Fix main. $worktree_rules"
  else
    note failed "VM test $c fails with $(describe) and passes on main"
    dispatch "check-$c" "$UPDATE: the VM test $c fails with these updates on top of main: $(describe). It passes on main, so the updates break it. The log is $log; the tree is $work at $head (don't edit there, it's reset every run). Make the updates pass it. $worktree_rules"
  fi
  if [ "$(age "check-$c")" -gt 86400 ]; then
    "$NOTIFY" open "check-$c" "Updates stuck: VM test $c fails" \
      "For a day now $UPDATE can't get the VM test $c to pass ($(describe)), so updates that don't fix a serious vulnerability aren't deployed. Claude jobs have tried; see \`proj status\` on $HOST."
  fi
  [ "$base" = fail ] || exit 1
done
if [ ${#untested[@]} -gt 0 ]; then
  fixes=
  if before=$(vulns "$current") && after=$(vulns "$new"); then
    fixes=$(comm -23 <(printf '%s\n' "$before") <(printf '%s\n' "$after"))
  else
    echo "vulnix couldn't scan both systems; treating it as fixing nothing"
  fi
  if [ -z "$fixes" ]; then
    note failed "VM test ${untested[*]} fails on main as well; holding $(describe) until main passes"
    exit 1
  fi
  echo "main fails ${untested[*]}, but this fixes: $(tr '\n' ',' <<<"$fixes")"
  "$NOTIFY" open untested "Security updates went live without a passing VM test" \
    "The VM test ${untested[*]} fails on main itself, and these updates fix serious vulnerabilities in what $HOST runs:
$fixes
So they went live without it, under the dead man's switch and the health check. A Claude job is fixing main's test; this closes when an update passes every test again."
fi

# 6. Live, with the dead man's switch; healthy, or straight back.
# Already live: it went through this when it did.
if [ -z "$live" ]; then
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
  [ ${#untested[@]} -gt 0 ] || settled untested "An update passed every VM test again."
fi

# 7. Push, and bring the operator's checkout along when it isn't mid-work.
# (The commit says what was skipped. The running system keeps the old commit
# as its revision; "deployed" below is how the next run knows it's this one.)
if [ ${#untested[@]} -gt 0 ] && [ -n "$changed" ] && [ -z "$riding" ]; then
  git_ commit -q --amend -m "deps: update $(describe | sed 's/, $//')" \
    -m "Pushed by $UPDATE without the VM test ${untested[*]}, which fails on main itself, because it fixes:
$fixes
It built and stayed healthy live."
fi
echo "$new" >"$state/deployed"
# What went live, in words: pushing can rebase it onto what hasn't.
went=${changed:+$(describe)}${changed:-main $(git_ rev-parse --short HEAD)}
if [ -n "$TRAIN" ]; then
  # Riding, or nothing new on main: nothing to put up.
  [ -n "$riding" ] || [ -z "$changed" ] || url=$(propose "$(git_ log -1 --format=%s)" "$(pr_body)") ||
    echo "its PR didn't open; the next run opens it"
  [ -n "$changed" ] || git_ rev-parse HEAD >"$state/deployed-main"
else
  push_main || true
fi
if [ "$(as_user git -C "$operator" branch --show-current 2>/dev/null)" = main ] &&
  as_user git -C "$operator" diff --quiet HEAD 2>/dev/null; then
  as_user git -C "$operator" pull -q --ff-only origin main || true
fi

note ok "${live:+already }live and confirmed: $went${riding:+ (riding #$riding)}${url:+ (up as $url)}${failed[*]:+ (held back: ${failed[*]})}"
[ -z "$BOOT" ] || ! "$BOOT" status | grep -q 'trial queued: /' ||
  echo "the new kernel boots once at the next trial window ($BOOT trial)"
exit 0
