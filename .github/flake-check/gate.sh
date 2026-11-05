# The shared CI's gate, flake-check.yml's gate step (and flake-gate's, as
# CI runs it): `nix flake check`, or with eval-apart each output evaluated in
# a nix process of its own, and with legs this leg's share of them. Its env
# is the step's: SKIP, APART, LEGS and LEG, and RUNNER_TEMP.
#
# GitHub runs a step's `shell: bash` with -eo pipefail.
set -eo pipefail
# How checks are built (build.sh, beside this).
# shellcheck source=build.sh
source "${BASH_SOURCE[0]%/*}/build.sh"
# The plan (plan.nix, beside this), spliced into each evaluation.
plan=$(< "${BASH_SOURCE[0]%/*}/plan.nix")
# Without eval-apart, the plan over one nix-instantiate's getFlake of
# the flake as locked, with whether it has a nixConfig.
read_plan() {
  url=$(nix flake metadata --json . | jq -r .url)
  args=(--option pure-eval true --argstr url "$url" --argstr system "$(nix config show system)" --argstr skip "$SKIP"
    --expr "{ url, system, skip, builds ? false }:
      let
        flake = builtins.getFlake url;
        planned = ($plan) { inherit system skip builds; } flake.outputs;
      in
      if builds then planned else planned // { nixConfig = import \"\${flake.outPath}/flake.nix\" ? nixConfig; }")
  nix-instantiate --eval --strict --json "${args[@]}" > "$RUNNER_TEMP/plan.json"
}
apart() {
  system=$(nix config show system)
  [[ $system =~ ^[A-Za-z0-9_-]+$ ]] || { echo "can't evaluate for system $system"; return 1; }
  work=$(mktemp -d "$RUNNER_TEMP/eval-apart.XXXXXX")
  trap 'rm -rf -- "$work"' EXIT
  # One snapshot of the checkout, as nix reads it: a git work tree's
  # tracked files as they are now, modes and all, its index, HEAD
  # and history (the repo's own objects, shared), its repository and
  # worktree settings, and each submodule checked out the same way
  # (any other left empty, as git leaves it), so self, clean or
  # dirty, is what nix flake check gives it. Everything here reads
  # the snapshot and nothing writes it (the lock file is used as it
  # is), so an edit to the checkout while the gate runs can't mix
  # two sources in one result.
  snapshot() {
    local top=$1 to=$2 common gitdir rev ref remote line f kv scope
    common=$(git -C "$top" rev-parse --path-format=absolute --git-common-dir)
    gitdir=$(git -C "$top" rev-parse --path-format=absolute --git-dir)
    git init -q --object-format="$(git -C "$top" rev-parse --show-object-format)" "$to"
    # The settings in effect there, the user's, the repository's and
    # the worktree's in that order, each with its includes (relative
    # and conditional ones too) resolved where the checkout is, and
    # frozen into the snapshot's own config, so the later ones still
    # win.
    : > "$to/.git/config"
    for scope in --global --local --worktree; do
      while IFS= read -r -d '' kv; do
        case $kv in
          *$'\n'*) set -- "${kv%%$'\n'*}" "${kv#*$'\n'}" ;;
          *) set -- "$kv" true ;;
        esac
        case ${1,,} in include.path | includeif.*.path | core.worktree | core.bare) continue ;; esac
        git config -f "$to/.git/config" --add "$1" "$2"
      done < <(git -C "$top" config "$scope" --includes --list -z 2> /dev/null)
    done
    git config -f "$to/.git/config" core.bare false
    # A relative submodule URL resolves against the default remote,
    # or with none against the work tree: the checkout's, not the
    # snapshot's.
    ref=$(git -C "$top" symbolic-ref -q HEAD) || ref=
    remote=$(git -C "$top" config "branch.${ref#refs/heads/}.remote") || remote=
    if ! git -C "$top" config "remote.${remote:-origin}.url" > /dev/null && ! git -C "$top" config remote.origin.url > /dev/null; then
      git config -f "$to/.git/config" remote.origin.url "$top"
    fi
    echo "$common/objects" > "$to/.git/objects/info/alternates"
    if [ -f "$common/shallow" ]; then cp -- "$common/shallow" "$to/.git/shallow"; fi
    if rev=$(git -C "$top" rev-parse -q --verify 'HEAD^{commit}'); then
      if [ -n "$ref" ]; then
        git -C "$to" symbolic-ref HEAD "$ref"
        git -C "$to" update-ref "$ref" "$rev"
      else
        git -C "$to" update-ref --no-deref HEAD "$rev"
      fi
    fi
    if [ -f "$gitdir/index" ]; then cp -- "$gitdir/index" "$to/.git/index"; fi
    git -C "$top" ls-files -z | (cd "$top" && while IFS= read -r -d '' f; do
      if [ -L "$f" ] || [ -f "$f" ]; then printf '%s\0' "$f"; fi
    done | tar --null --no-recursion -T - -cf -) | tar -xpf - -C "$to"
    while IFS= read -r -d '' line; do
      f=${line#*$'\t'}
      if [ -e "$top/$f/.git" ]; then snapshot "$top/$f" "$to/$f" < /dev/null; else mkdir -p -- "$to/$f"; fi
    done < <(git -C "$top" ls-files -z -s | grep -z '^160000 ')
  }
  source=$work/source
  if top=$(git rev-parse --show-toplevel 2> /dev/null); then
    snapshot "$top" "$source"
    flake=$source/$(git rev-parse --show-prefix)
  else
    cp -a . "$source"
    flake=$source
  fi
  # nix flake check applies the nixConfig a runner accepts; the
  # evaluations and the build here would leave it out.
  if [ "$(nix-instantiate --eval --argstr f "$flake/flake.nix" --expr '{ f }: (import f) ? nixConfig')" = true ]; then
    echo "eval-apart can't carry this flake's nixConfig to every evaluation and build. Its gate is a whole \`nix flake check\`, without eval-apart."
    return 1
  fi
  # The flake's outputs ("#."), as nix flake check evaluates them,
  # each time afresh.
  evaluate() { nix eval --no-update-lock-file --option eval-cache false --apply "($plan) { system = \"$system\"; $1 }" "${@:2}" "$flake#."; }
  evaluate "" --json > "$RUNNER_TEMP/plan.json"
  if [ "$(jq '.other | length' "$RUNNER_TEMP/plan.json")" != 0 ]; then
    echo "eval-apart doesn't check apps, overlays, hydraJobs, bundlers or the old default* outputs, and this flake has $(jq -c .other "$RUNNER_TEMP/plan.json"). Its gate is a whole \`nix flake check\`, without eval-apart."
    return 1
  fi
  jq -r '.unknown[] | "warning: unknown flake output \u0027\(.)\u0027"' "$RUNNER_TEMP/plan.json"
  # Which entries are checks to build, which checks are skipped, and
  # which entries are this leg's: those whose names hash to it. By
  # name, not place in the plan, so an output added or gone moves no
  # other to a leg whose kept builds (keep-builds) lack it.
  jq --arg skip "$SKIP" --argjson legs "$LEGS" --argjson leg "$LEG" 'def hash: reduce explode[] as $c (0; (. * 31 + $c) % 1000003);
    ($skip | split(" ") | map(select(. != ""))) as $s
    | .entries | map(.check and (.name as $n | $s | index([$n]) | not)) as $build
    | [range(length) as $i | .[$i] + {build: $build[$i], skip: (.[$i].check and ($build[$i] | not)),
        mine: (.[$i].what | hash % $legs == $leg - 1)}]' \
    "$RUNNER_TEMP/plan.json" > "$RUNNER_TEMP/entries.json"
  if [ "$LEGS" -gt 1 ]; then
    echo "leg $LEG of $LEGS: $(jq 'map(select(.mine)) | length' "$RUNNER_TEMP/entries.json") of the plan's $(jq length "$RUNNER_TEMP/entries.json") outputs, those whose names hash to it"
  fi
  # Evaluations run side by side, one a core while each has 2G of
  # the memory free to this job (one of burrow's took at most
  # 1.3G), so the most memory in use is that many evaluations';
  # each one's log is printed whole, in the plan's order, once
  # they've all ended. Each derivation is made a GC root at once,
  # until the build is done; one that isn't a valid derivation (or
  # that a GC took in between) fails the gate. A failed evaluation
  # fails the gate, after the rest are evaluated and built
  # (--keep-going).
  roots=$work/roots
  evals=$work/evals
  mkdir "$roots" "$evals"
  side_by_side
  # One entry's evaluation, with what came of it in $evals/N: a
  # derivation to build, or ok; nothing if it failed.
  one() {
    local i=$1 what result drv
    what=$(jq -r ".[$i].what" "$RUNNER_TEMP/entries.json")
    echo "evaluating $what"
    if ! result=$(evaluate "entry = $i;" --json); then
      return
    elif [ "$(jq ".[$i].nixos" "$RUNNER_TEMP/entries.json")" = true ]; then
      :
    elif ! drv=$(jq -er .drv <<< "$result") || ! nix-instantiate --add-root "$roots/$i" --argstr drv "$drv" \
        --expr '{ drv }: { type = "derivation"; name = "root"; outputName = "out"; drvPath = drv; }' > /dev/null; then
      echo "evaluating $what gave no derivation"
      return
    elif [ "$(jq ".[$i].build" "$RUNNER_TEMP/entries.json")" = true ]; then
      echo "$drv^*" > "$evals/$i"
      return
    elif [ "$(jq ".[$i].skip" "$RUNNER_TEMP/entries.json")" = true ]; then
      echo "not building $what"
    fi
    echo ok > "$evals/$i"
  }
  # Listed to a file first, so a jq that fails fails the gate
  # (a pipe into mapfile would hide it and evaluate nothing).
  jq -r 'to_entries[] | select(.value.mine) | .key' "$RUNNER_TEMP/entries.json" > "$work/mine"
  mapfile -t mine < "$work/mine"
  echo "${#mine[@]} outputs to evaluate, $side side by side"
  for i in "${mine[@]}"; do
    while [ "$(jobs -pr | wc -l)" -ge "$side" ]; do wait -n || true; done
    one "$i" > "$evals/$i.log" 2>&1 &
  done
  wait
  failed=0
  targets=()
  for i in "${mine[@]}"; do
    cat "$evals/$i.log"
    if [ ! -f "$evals/$i" ]; then
      failed=1
    elif [ "$(cat "$evals/$i")" != ok ]; then
      targets+=("$(cat "$evals/$i")")
    fi
  done
  if [ ${#targets[@]} -gt 0 ]; then
    build_checks "" "${targets[@]}" || failed=1
  fi
  return $failed
}
# Without eval-apart, the gate is nix flake check, as nix runs it. A
# nix that builds what it evaluated holds every evaluation in memory
# until its last build ends: burrow's checks took 12.6G, and beside
# its VM tests on a 16G hosted runner the runner was shut down. So the
# checks are built first, by derivation: one nix-instantiate evaluates
# them and makes each a GC root, and once it has exited nix build
# builds every output, as nix flake check would, which then finds them
# built. A flake with a nixConfig, or whose plan can't be read, is
# left to nix flake check alone: only nix applies the config a runner
# accepts, and says what's wrong with a flake.
whole() {
  failed=0
  built=false
  if read_plan && [ "$(jq .nixConfig "$RUNNER_TEMP/plan.json")" = false ]; then
    roots=$(mktemp -d "$RUNNER_TEMP/roots.XXXXXX")
    trap 'rm -rf -- "$roots"' EXIT
    targets=()
    if nix-instantiate --add-root "$roots/check" --arg builds true "${args[@]}" > "$roots/list"; then
      # A derivation whose first output isn't out has its root
      # printed with that output's name after a !. What's built
      # stays a root too, until nix flake check has found it.
      while read -r root; do targets+=("$(readlink "${root%%!*}")^*"); done < "$roots/list"
      if [ ${#targets[@]} -eq 0 ] || build_checks "$roots/out" "${targets[@]}"; then
        built=true
      else
        # Built once is enough: nix flake check only says what else
        # is wrong, rather than build them again beside every
        # evaluation.
        nix flake check --no-build -L --keep-going || true
        return 1
      fi
    else
      # nix flake check says which, and builds the rest.
      failed=1
    fi
  fi
  # What's built already needs no slot. What isn't is nix's to build
  # by name, with no telling the VM tests apart: all of it in a slot.
  if [ -z "$SKIP" ]; then
    if [ "$built" = true ]; then
      nix flake check -L --keep-going || failed=1
    else
      in_slot nix flake check -L --keep-going || failed=1
    fi
    return $failed
  fi
  nix flake check --no-build -L || return 1
  if [ "$built" = true ]; then
    for c in $SKIP; do echo "not building $c"; done
    return $failed
  fi
  system=$(nix eval --impure --raw --expr builtins.currentSystem)
  targets=()
  for c in $(nix eval --raw ".#checks.$system" --apply 'cs: toString (builtins.attrNames cs)'); do
    case " $SKIP " in *" $c "*) echo "not building $c"; continue ;; esac
    targets+=(".#checks.$system.$c")
  done
  in_slot nix build -L --no-link --keep-going "${targets[@]}" || failed=1
  return $failed
}
gate() {
  # Legs: 1 to 8, each of them evaluating apart.
  if ! [[ $LEGS =~ ^[1-8]$ && $LEG =~ ^[1-8]$ ]] || [ "$LEG" -gt "$LEGS" ]; then
    echo "can't run leg $LEG of $LEGS: there are 1 to 8 legs, numbered from 1"
    return 1
  elif [ "$LEGS" -gt 1 ] && [ "$APART" != true ]; then
    echo "legs need eval-apart: without it, the gate is one nix flake check, which can't be split"
    return 1
  fi
  if [ "$APART" = true ]; then
    apart
  else
    whole
  fi
}
gate 2>&1 | tee "$RUNNER_TEMP/flake.log"
