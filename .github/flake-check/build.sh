# Building checks, for the shared CI's gate (gate.sh) and its negative run
# (negative.sh), which source this: on a machine with heavy's slots, a check
# with a VM test to run builds only while it holds one. The caller sets
# roots, a directory the VM tests' outputs are rooted in while it runs.
#
# GitHub runs a step's `shell: bash` with -eo pipefail, and so do they.
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
