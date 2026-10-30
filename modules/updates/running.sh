# What HOST runs, against what HEAD builds. Every system is stamped with the
# commit it's built from (its configuration revision), so two builds of the
# same tree from different commits are different store paths, and the stamp
# alone doesn't say which commits are live. On its own, ahead of update.sh, so
# a check covers it (tests/update-running.sh); update.sh sets work, CONFIG,
# state, current, rev, riding, changed, as_user and git_.
# shellcheck disable=SC2154

# Is HEAD's system the one running, apart from the commit it's stamped with?
# (A build from a dirty checkout, then committed, differs only there.)
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

# Whether $1, HEAD's system, runs already: that very system, or, when HEAD is
# a commit this run made (a bump, or the merge that rides its PR on main),
# the same one apart from its stamp. Each run makes that commit afresh, at
# its own time, so its stamp never matches the one an earlier run put live,
# even when nothing that runs differs. (Of HEAD as main itself, the run asked
# before building, unless it's forced.)
runs_already() {
  [ "$1" = "$current" ] || { [ -n "$changed" ] && same_as_running; }
}

# HEAD builds $1, the system HOST runs already. "up" when it's a bump of
# HOST-update's own to put up (as a PR in a train repo, to main otherwise),
# not riding: only once HOST-update put that system live itself (its record
# in deployed), through the switch and the health check, and then only after
# the VM tests on HEAD's tree. "hold" when someone else did. Otherwise
# "current": nothing to do.
already_live() {
  if [ -n "$riding" ] || [ -z "$changed" ]; then
    echo current
  elif [ "$(cat "$state/deployed" 2>/dev/null)" = "$1" ]; then
    echo up
  else
    echo hold
  fi
}

# Every commit of main up to $1 is in $2, the system that runs: what a run
# found, for burrow-dispatch, which counts main's commits past it as not live
# yet. The stamp can't say it: a system that runs already keeps an older
# commit's, and a ride's is a merge never pushed. It names the system, so a
# switch to another since, by anyone, leaves it saying nothing.
live_at() {
  jq -n --arg main "$1" --arg running "$2" '{$main, $running}' >"$state/live.json.new" &&
    mv "$state/live.json.new" "$state/live.json"
}
