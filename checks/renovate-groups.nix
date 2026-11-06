# The branches a repo's updates get under the example's config, with
# config:recommended's presets resolved as a run resolves them
# (tests/renovate-groups.mjs): every minor in the one group, a monorepo's
# and one Renovate's own presets would group included, and majors apart, a
# source repo's together.
{ self, pkgs, ... }:
pkgs.runCommand "renovate-groups-check" { } ''
  export HOME=$TMPDIR
  got=$(LOG_LEVEL=fatal ${pkgs.lib.getExe pkgs.nodejs_24} ${../tests/renovate-groups.mjs} ${pkgs.renovate} \
    ${self.nixosConfigurations.example.config.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE} 2>/dev/null)
  want='renovate/dependencies: @babel/core minor, @babel/preset-env minor, @types/node minor, puppeteer minor, react minor, serde minor, tokio minor
  renovate/left-pad-2.x: left-pad major
  renovate/major-jestjs-jest: @jest/globals major, jest major'
  [ "$got" = "$(sed 's/^ *//' <<<"$want")" ] || { echo "got: $got"; exit 1; }
  touch $out
''
