# A fixture machine's `proj settings show --json`, with values from its
# flake and from each live file, validates against the JSON Schema
# burrow-settings generates (`proj settings schema`), checked by a validator
# of its own; and a document with an option the schema doesn't declare, or
# a value from where its option can't come, doesn't.
{ self, pkgs, ... }:
let
  fixture = self.nixosConfigurations.example.extendModules {
    modules = [ {
      burrow.autoland.rounds = 3;
      burrow.proj.maxJobs = 6;
      burrow.proj.paused = [ "sam/kept" ];
    } ];
  };
  proj = fixture.config.burrow.tools.proj;
in
pkgs.runCommand "settings-check" { nativeBuildInputs = [ pkgs.jq pkgs.check-jsonschema ]; } ''
  # The fixture's own config, its live files moved where the build can write.
  config=$(grep -o '/nix/store/[^ ]*-proj\.json' ${proj}/bin/proj | head -n1)
  live=$TMPDIR/live
  mkdir -p $live/jobs $live/config/roles.d
  jq --arg live "$live" '.jobs = "\($live)/jobs" | .roles = "\($live)/config/roles" | .models = "\($live)/config/models"' \
    "$config" >proj.json
  printf 'triage codex\nfix@sam/homelab codex over 80 claude\n' >$live/config/roles.d/usual
  ln -s $live/config/roles.d/usual $live/config/roles
  printf 'triage codex - low\n' >$live/config/models
  now=$(date +%s)
  printf '{"until": %d, "said": "out of usage", "job": "j1", "at": %d, "told": %d}' \
    $((now + 3600)) $now $now >$live/jobs/.usage.json
  run() { PROJ_CONFIG=$PWD/proj.json PROJ_SELF=${proj}/bin/proj ${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proj}/bin/burrow-proj "$@"; }

  run settings schema >schema.json
  run settings show --json >show.json
  run settings show >show.txt
  cat show.txt
  check-jsonschema --schemafile schema.json show.json

  want() { got=$(jq -cS "$1" show.json); [ "$got" = "$(echo "$2" | jq -cS .)" ] || { echo "$1: $got, not $2"; exit 1; }; }
  want '.options["autoland.rounds"]' '{"value":3,"origin":"flake"}'
  want '.options["proj.maxJobs"]' '{"value":6,"origin":"flake"}'
  want '.options["autoland.jobIdleMinutes"]' '{"value":20,"origin":"default"}'
  want '.options["proj.paused"].value' '["sam/kept"]'
  want '.options["proj.roles"].value' '{"triage":"codex","fix@sam/homelab":"codex over 80 claude"}'
  want '.options["proj.models"].value' '{"triage codex":{"model":null,"effort":"low"}}'
  want '.options["proj.engineSetups"].value.active' '"usual"'
  want '.options["proj.usageHold"].origin' '"live"'
  want '.options | length' "$(jq '.properties.options.required | length' schema.json)"

  # What the schema is for: it refuses an option it doesn't declare, a value
  # of the wrong kind, and a flake option said to come from a live file.
  refuses() {
    jq "$1" show.json >bad.json
    if check-jsonschema --schemafile schema.json bad.json; then echo "the schema took: $1"; exit 1; fi
  }
  refuses '.options["proj.undeclared"] = {"value": 1, "origin": "flake"}'
  refuses '.options["autoland.rounds"].value = "3"'
  refuses '.options["autoland.rounds"].origin = "live"'
  refuses 'del(.options["proj.roles"])'
  touch $out
''
