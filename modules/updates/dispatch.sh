# Hand a problem to a Claude job in the machine's own project, once per problem
# while one is still working on it (or did in the last 6 hours). Fixing what
# deploys everything is work on the pipeline itself (proj run --pipeline): it
# goes ahead of everyone's jobs, in the slot kept for it, never behind a
# project's long ones: proj counts the machine's flake as the pipeline's own
# (proj's pick), with or without autoland's pipelineRepos. On its own, ahead of
# update.sh, so a check covers it (tests/update-dispatch.sh); update.sh sets
# state, PROJECT, APP and as_user. It acts on GitHub as the machine's App,
# APP, when it has one.
# shellcheck disable=SC2154
dispatch() {
  local prompt=$2 f="$state/jobs/$1" id
  if [ -s "$f" ] && [ $(($(date +%s) - $(stat -c %Y "$f"))) -lt 21600 ]; then
    return 0
  fi
  if id=$(as_user proj run --pipeline ${APP:+--app "$APP"} "$PROJECT" "$prompt"); then
    echo "$id" >"$f"
    echo "handed to Claude: proj log $id"
  fi
}
