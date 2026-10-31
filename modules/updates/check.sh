# VM test $1 on commit $2, logged to $3, in one of the machine's heavy slots
# when it has them (HEAVY). A VM test beside the machine's other gates and VM
# tests ran ten times its usual length and past the test driver's hour; waiting
# for a slot happens before the build starts, so it doesn't use up that hour.
run_check() {
  local slot=()
  [ -z "$HEAVY" ] || slot=("$HEAVY" --)
  as_user "${slot[@]}" nix build --no-link -L "git+file://$work?rev=$2#checks.$SYSTEM.$1" >"$3" 2>&1
}
