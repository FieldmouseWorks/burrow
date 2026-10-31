# renovate: Renovate over every repo it covers, or over one.
#
#   renovate              every repo (burrow-events' catch-up: new releases
#                         upstream send no event)
#   renovate OWNER/REPO   that repo, if a full run covers it: any repo's
#                         events reach this unit
#
# As the machine's own App (APP_ENV, its env), a token is one owner's
# installation's and good for fifty minutes from when it's asked for
# (gh-token), so each repo gets a run of its own with its own token, and a
# full one is every repo the installations of APP_OWNERS reach. As the user,
# one run with gh's login covers them all. Runs take RUN_LOCK in turn: they
# share the clones. A run's token is asked for once it has the lock, so its
# fifty minutes aren't spent waiting on another's. Each run that ends,
# failed or not, is noted in RUNS (ran). (APP_ENV, APP_OWNERS, COVERS,
# RUN_LOCK and RUNS come from ../updates.nix.)
only=${1:-}

if [ -n "$APP_ENV" ]; then
  # shellcheck source=/dev/null
  . "$APP_ENV"
fi

# The token for a run on REPO.
token() {
  if [ -n "$APP_ENV" ]; then
    gh-token get "${1%%/*}"
  else
    gh auth token
  fi
}

# When a run on NAME ended: the last ten, in a file named for the repo in
# lower case with its / as _ (`all` for one run over every repo), for
# autoland's dashboard watcher, which hands on an error still on a repo's
# dashboard after two of them.
ran() {
  local f=${1,,}
  f=$RUNS/${f//\//_}
  mkdir -p "$RUNS"
  { tail -n 9 "$f" 2>/dev/null || true; date +%s; } > "$f.tmp"
  mv "$f.tmp" "$f"
}

# Renovate with ARGS, on REPO (or every repo, as the user), in its turn.
run() {
  local repo=$1
  shift
  (
    flock 9 || exit
    t=$(token "$repo") || exit
    rc=0
    RENOVATE_TOKEN=$t GITHUB_COM_TOKEN=$t renovate "$@" || rc=$?
    ran "${repo:-all}"
    exit "$rc"
  ) 9>>"$RUN_LOCK"
}

# One repo. Renovate's own code applies its autodiscoverFilter here (COVERS),
# to the repo as GitHub spells it (burrow-events has it in lower case, and a
# /regex/ filter can mind case). Then its own autodiscovery, narrowed to this
# repo, applies the rest (topics, forks, archived, mirrors).
one() {
  local repo covers=0
  repo=$(gh api "repos/$1" --jq .full_name) || return 1
  "$COVERS" "$repo" || covers=$?
  case $covers in
    0) ;;
    10) echo "$repo: not one of Renovate's (autodiscoverFilter)"; return 0 ;;
    *) echo "$repo: couldn't apply autodiscoverFilter" >&2; return 1 ;;
  esac
  run "$repo" --autodiscover-filter="[\"$repo\"]"
}

if [ -n "$only" ]; then
  one "$only"
  exit
fi

if [ -z "$APP_ENV" ]; then
  run ''
  exit
fi

# Every repo the installations reach, a run each. One that fails doesn't
# hold up the rest, but fails the pass.
failed=0
for owner in $APP_OWNERS; do
  if ! repos=$(GH_TOKEN_OWNER=$owner gh api installation/repositories --paginate \
    --jq '.repositories[] | select(.archived | not) | .full_name'); then
    echo "couldn't list the repos $owner's installation reaches" >&2
    failed=1
    continue
  fi
  for repo in $repos; do
    one "$repo" || { echo "$repo: Renovate's run failed" >&2; failed=1; }
  done
done
exit "$failed"
