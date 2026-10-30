# Put what just went live on main. Main moves while an update runs (its VM
# tests can outlast the gap between merges), so a push that gives up on the
# first refusal can lose every bump of a busy night while each one goes live.
# It goes on top of what landed instead. The commit that went live is the only
# one that marks main as running it: one rebased here also carries what landed
# meanwhile, which the push's own run builds, tests and deploys. On its own,
# ahead of update.sh, so a check covers it (tests/update-push.sh); update.sh
# sets state and git_.
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
  done
  echo "push to main was refused $try times; the next run bumps it afresh"
  return 1
}
