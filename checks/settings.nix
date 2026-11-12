# A fixture machine's `proj settings show --json`, with values from its
# flake and from the live overlay (proj's old files moved into it once),
# validates against the JSON Schema burrow-settings generates (`proj
# settings schema`), checked by a validator of its own; and a document with an option the schema doesn't declare, or
# a value from where its option can't come, doesn't. A module default that
# isn't the schema's, hidden by the flake's own value, fails the show.
{ self, pkgs, ... }:
let
  fixture = self.nixosConfigurations.example.extendModules {
    modules = [ ({ config, ... }: {
      burrow.autoland.rounds = 3;
      # An override that runs proj itself: proj's own config can't carry the
      # flake's values, or this is a cycle.
      burrow.autoland.repos."sam/kept".proofRuns.check = "${config.burrow.tools.proj}/bin/proj check-config";
      burrow.proj.maxJobs = 6;
      burrow.settings.allowed = [ "sam/kept" ];
    }) ];
  };
  proj = fixture.config.burrow.tools.proj;
  tracked = fixture.config.environment.etc."burrow/settings.json".source;
in
pkgs.runCommand "settings-check" { nativeBuildInputs = [ pkgs.jq pkgs.check-jsonschema ]; } ''
  # The fixture's own config, its live files moved where the build can write.
  config=$(grep -o '/nix/store/[^ ]*-proj\.json' ${proj}/bin/proj | head -n1)
  live=$TMPDIR/live
  mkdir -p $live/jobs $live/config
  [ "$(jq -r .trackedSettings "$config")" = /etc/burrow/settings.json ]
  jq --arg live "$live" --arg pwd "$PWD" \
    '.jobs = "\($live)/jobs" | .oldConfig = "\($live)/config" | .settings.overlay = "\($live)/settings/overlay.json" | .trackedSettings = "\($pwd)/settings.json"' \
    "$config" >proj.json
  jq . ${tracked} >settings.json
  now=$(date +%s)
  printf '{"until": %d, "said": "out of usage", "job": "j1", "at": %d, "told": %d}' \
    $((now + 3600)) $now $now >$live/jobs/.usage.json
  run() { PROJ_CONFIG=$PWD/proj.json PROJ_SELF=${proj}/bin/proj ${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proj}/bin/burrow-proj "$@"; }
  # The old files, moved into the settings once; then each live value
  # through the door.
  printf 'triage codex\n' >$live/config/roles
  printf 'triage codex - low\n' >$live/config/models
  # Unmoved, proj refuses role settings rather than run the defaults.
  if run check-config 2>err; then echo "check-config took unmoved files"; exit 1; fi
  grep -F "aren't moved into the settings yet" err
  run settings migrate
  run check-config
  # The old files stay, for a generation from before the settings.
  [ -e $live/config/roles ]
  run settings role fix@sam/homelab codex over 80 claude
  run engines save usual

  run settings schema >schema.json
  run settings show --json >show.json
  run settings show >show.txt
  cat show.txt
  check-jsonschema --schemafile schema.json show.json

  want() { got=$(jq -cS "$1" show.json); [ "$got" = "$(echo "$2" | jq -cS .)" ] || { echo "$1: $got, not $2"; exit 1; }; }
  want '.options["autoland.rounds"]' '{"value":3,"origin":"flake"}'
  want '.options["proj.maxJobs"]' '{"value":6,"origin":"flake"}'
  want '.options["autoland.jobIdleMinutes"]' '{"value":20,"origin":"default"}'
  want '.options["settings.allowed"].value' '["sam/kept"]'
  want '.options["proj.roles"].value' '{"*":{"engine":"claude","over":null},"planner":{"engine":"codex","over":null},"triage":{"engine":"codex","over":null},"fix@sam/homelab":{"engine":"codex","over":{"pct":80.0,"engine":"claude"}}}'
  want '.options["proj.models"].value' '{"* claude":{"model":null,"effort":null},"* codex":{"model":null,"effort":null},"triage codex":{"model":null,"effort":"low"}}'
  want '.options["proj.roles"].file' "\"$live/settings/overlay.roles.json\""
  want '.options["proj.engineSetups"].value.active' '"usual"'
  want '.options["proj.engineOffers"].origin' '"default"'
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

  # The flake overrides rounds, but its module's own default still has to be
  # the schema's.
  jq '.["autoland.rounds"].default = 4' ${tracked} >settings.json
  if run settings show 2>err; then echo "show took a module default of 4"; exit 1; fi
  grep -F "autoland.rounds: its module's default is 4, the settings schema's is 5" err
  touch $out
''
