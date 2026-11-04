# The shared CI's gate, flake-check.yml's gate step (and flake-gate's, as
# CI runs it): `nix flake check`, or with eval-apart each output evaluated in
# a nix process of its own, and with legs this leg's share of them. Its env
# is the step's: SKIP, APART, LEGS and LEG, and RUNNER_TEMP.
#
# GitHub runs a step's `shell: bash` with -eo pipefail.
set -eo pipefail
# The plan (plan.nix, beside this), spliced into each evaluation.
plan=$(< "${BASH_SOURCE[0]%/*}/plan.nix")
# On a machine with heavy's slots (burrow.cpu: self-hosted, never a
# hosted runner), a VM test builds only while it holds one: beside
# the jobs holding them, one ran past the test driver's hour. A
# slot's share of the machine is HEAVY_JOBS threads, so that's as
# many builds as it starts.
has_slots() { command -v heavy > /dev/null; }
in_slot() {
  if has_slots; then
    # shellcheck disable=SC2016 # bash -c's own "$@", not this one's
    heavy -- bash -c 'exec "$@" --max-jobs "$HEAVY_JOBS"' bash "$@"
  else
    "$@"
  fi
}
# Prints which derivations in these ones' closures still have a VM
# to run, by store name: those requiring kvm or nixos-test with an
# output it couldn't root as it stands. A rerun's tests, already
# built, wait for nothing, and stay built under $roots until the
# gate's end; rooting a path builds nothing. Nix's JSON has
# structured attrs as structuredAttrs or, before 2.25 or so, in
# env.__json, and anything else with no env is an error.
unbuilt_vms() {
  local name out outs i=0 pending
  nix derivation show -r "${@/%/^*}" | jq -r '(.derivations // .) | to_entries[]
    | (.value.env | if type != "object" then error("a derivation with no env") else . end) as $env
    | select(($env.requiredSystemFeatures // "" | [splits("[ \t\n\r]+")])
        + (.value.structuredAttrs.requiredSystemFeatures // [])
        + ($env.__json // "{}" | fromjson | .requiredSystemFeatures // [])
      | any(. == "kvm" or . == "nixos-test"))
    | .key | sub(".*/"; "")' > "$RUNNER_TEMP/vms" || return 1
  while read -r name; do
    pending=
    outs=$(nix-store -q --outputs "${1%/*}/$name") || pending=1
    [ -n "$outs" ] || pending=1
    for out in $outs; do
      i=$((i + 1))
      nix-store --add-root "$roots/vm-output-$i" --indirect -r "$out" > /dev/null 2>&1 || pending=1
    done
    [ -z "$pending" ] || echo "$name"
  done < "$RUNNER_TEMP/vms"
}
# Builds these checks (each a derivation and ^*), keeping the outputs
# under the link $1 if it's given: first those with no VM to run in
# their closures, with no slot, then the rest in one. Both are built
# by derivation, so neither holds an evaluation. A check that can't
# be told apart counts as a VM test.
build_checks() {
  local link=$1 rc=0 target closure found plain=() vms=() plain_link=(--no-link) vm_link=(--no-link)
  shift
  [ -z "$link" ] || { plain_link=(--out-link "$link"); vm_link=(--out-link "$link-vm"); }
  if ! has_slots; then
    nix build -L "${plain_link[@]}" --keep-going "$@"
    return
  fi
  if ! unbuilt_vms "${@%^\*}" > "$RUNNER_TEMP/unbuilt-vms"; then
    echo "couldn't tell which checks are VM tests, so all of them take the slot"
    vms=("$@")
  else
    for target in "$@"; do
      # grep's 1 is no match, and only that's no VM.
      found=2
      if closure=$(nix-store -q -R "${target%^\*}"); then
        found=0
        grep -qFf "$RUNNER_TEMP/unbuilt-vms" <<< "$closure" || found=$?
      fi
      if [ "$found" = 1 ]; then plain+=("$target"); else vms+=("$target"); fi
    done
  fi
  if [ ${#plain[@]} -gt 0 ]; then
    nix build -L "${plain_link[@]}" --keep-going "${plain[@]}" || rc=1
  fi
  if [ ${#vms[@]} -gt 0 ]; then
    echo "waiting for a heavy slot for ${#vms[@]} checks with VM tests"
    in_slot nix build -L "${vm_link[@]}" --keep-going "${vms[@]}" || rc=1
  fi
  return $rc
}
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
  # Free to this job: the machine's available memory, or less where
  # its memory cgroup, or one above that, has a limit (a proj job's
  # or a Claude server's MemoryMax, a container's): what the limit
  # leaves, counting as free the inactive page cache the kernel
  # gives back first. The cgroup is v1's memory controller's when
  # the job has one, cgroup v2's otherwise, read through the mount
  # of its hierarchy that shows the most cgroups above it, however
  # it's mounted (with other v1 controllers, from a subtree). A
  # limit that can't be read (no mount shows the cgroup, or a
  # cgroup with a limit doesn't say what it holds) is unknown, so
  # one evaluation at a time. CGROUP_PROC is where the job's
  # cgroups and mounts are read from (/proc/self; a test's
  # stand-in).
  free=$(( $(awk '$1 == "MemAvailable:" { print $2 }' /proc/meminfo) * 1024 ))
  proc=${CGROUP_PROC:-/proc/self}
  kind='' at='' top='' unknown=''
  # The hierarchy, then the job's cgroup's directory and the
  # mount's, a line each; or the hierarchy and "-" when no mount
  # shows the cgroup. Mount points are unescaped (mountinfo writes
  # a space \040, a tab \011); one that isn't stays unfound.
  # No cgroup file at all is a kernel without cgroups; one that's
  # there but can't be read (or its mounts) is an unknown limit.
  if [ -e "$proc/cgroup" ]; then
    if [ -f "$proc/cgroup" ] && [ -f "$proc/mountinfo" ] && awk '
      function unescape(s) { gsub(/\\040/, " ", s); gsub(/\\011/, "\t", s); return s }
      FILENAME == ARGV[1] {
        controllers = $0; sub(/^[^:]*:/, "", controllers)
        path = controllers; sub(/:.*/, "", controllers); sub(/^[^:]*:/, "", path)
        if (("," controllers ",") ~ /,memory,/) { kind = "v1"; job = path }
        else if (controllers == "" && kind == "") { kind = "v2"; job = path }
        next
      }
      {
        for (i = 7; i < NF && $i != "-"; i++) { }
        if (kind == "v2" && $(i + 1) != "cgroup2") next
        if (kind == "v1" && ($(i + 1) != "cgroup" || ("," $(i + 3) ",") !~ /,memory,/)) next
        root = unescape($4)
        if (root == "/") root = ""
        if (job != root && index(job, root "/") != 1) next
        if (found && length(root) >= length(best)) next
        best = root; mount = unescape($5); found = 1
      }
      END {
        if (kind == "") exit
        print kind
        if (!found) { print "-"; exit }
        print mount substr(job, length(best) + 1)
        print mount
      }
    ' "$proc/cgroup" "$proc/mountinfo" > "$work/memory-cgroup"; then
      { IFS= read -r kind; IFS= read -r at; IFS= read -r top; } < "$work/memory-cgroup" || true
    else
      unknown="its cgroups couldn't be read"
    fi
  fi
  case $kind in
    v1) max=memory.limit_in_bytes holds=memory.usage_in_bytes cache=total_inactive_file ;;
    v2) max=memory.max holds=memory.current cache=inactive_file ;;
  esac
  if [ -n "$kind" ] && [ -z "$unknown" ]; then
    if [ "$at" = - ]; then
      unknown="its memory cgroup ($kind) isn't mounted here"
    elif [ ! -d "$at" ]; then
      unknown="its memory cgroup ($kind) isn't at $at"
    else
      # The job's cgroup and each above it to the mount's, each
      # limit (none past 2^62, v1's "none") less what it holds.
      at=${at%/} top=${top%/}
      # A cgroup without the limit's file has none (the root, or
      # one whose parent doesn't hand it the controller); one
      # whose file is there but doesn't read as a number or "max"
      # has a limit nobody can read.
      number() { case $1 in '' | *[!0-9]*) return 1 ;; esac; }
      while [ -z "$unknown" ]; do
        if [ -e "$at/$max" ]; then
          limit=$(cat "$at/$max") || limit=''
          if [ "$limit" = max ]; then
            :
          elif ! number "$limit"; then
            unknown="$at/$max can't be read"
          elif [ "$limit" -lt $(( 1 << 62 )) ]; then
            if held=$(cat "$at/$holds") && inactive=$(awk -v k="$cache" '$1 == k { print $2; got = 1 } END { exit !got }' "$at/memory.stat") \
                && number "$held" && number "$inactive"; then
              left=$(( limit - held + inactive ))
              [ "$left" -ge "$free" ] || free=$left
            else
              unknown="$at has a limit but doesn't say what it holds"
            fi
          fi
        fi
        [ -n "$at" ] && [ "$at" != "$top" ] || break
        at=${at%/*}
      done
    fi
  fi
  if [ -n "$unknown" ]; then
    echo "this job's memory limit is unknown ($unknown): one evaluation at a time"
    free=0
  fi
  side=$(nproc)
  room=$(( free / 2147483648 ))
  [ "$room" -ge "$side" ] || side=$room
  [ "$side" -ge 1 ] || side=1
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
