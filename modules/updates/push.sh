# Put what just went live on main. Main moves while an update runs (its VM
# tests can outlast the gap between merges), so a push that gives up on the
# first refusal can lose every bump of a busy night while each one goes live.
# It goes on top of what landed instead, once that builds: a clean rebase can
# still not. What went live is the only commit that marks main as running it:
# one rebased here also carries what landed meanwhile, which the push's own run
# puts through the VM tests and the health check before anything goes live. On
# its own, ahead of update.sh, so a check covers it (tests/update-push.sh);
# update.sh sets state, git_ and build.
# shellcheck disable=SC2154
push_main() {
  local live try
  live=$(git_ rev-parse HEAD)
  for try in 1 2 3; do
    if git_ push -q origin HEAD:main; then
      [ "$(git_ rev-parse HEAD)" != "$live" ] || echo "$live" >"$state/deployed-main"
      return 0
    fi
    git_ fetch -q origin main || break
    if ! git_ rebase -q origin/main; then
      git_ rebase --abort || true
      echo "main moved and this update doesn't go on top of it (try $try); the next run bumps it afresh"
      return 1
    fi
    # Deploying main itself: nothing of its own to put on top.
    [ "$(git_ rev-parse HEAD)" != "$(git_ rev-parse origin/main)" ] || return 0
    if [ -z "$(build)" ]; then
      echo "main moved and this update doesn't build on top of it; the next run bumps it afresh"
      return 1
    fi
  done
  echo "push to main was refused $try times; the next run bumps it afresh"
  return 1
}
