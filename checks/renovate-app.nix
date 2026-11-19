# Renovate's App (burrow.updates.renovate.app, modules/updates/renovate-identity.nix)
# on the example machine. Unset, it's the machinery's App, and renovate-run's
# config is the one that App's env and gh make, as before; gitAuthor derives
# to that App's bot. Set to an App of its own: renovate-run acts as that App
# and nothing Renovate runs names the machinery's; gitAuthor derives to its
# bot; renovate-sweep's jobs still act as the machinery while it reads
# Renovate's word by the new bot; and the steward (autoland.json, burrow-upkeep's
# config) knows Renovate's PRs and commits by that bot while still trusting the
# machinery's. A gitAuthor naming any other bot, or an App that isn't one of
# burrow.github.apps, fails evaluation, and so does Renovate as the user
# beside a machinery App, and a gitAuthor in Renovate's settings that isn't
# the option's.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  example = self.nixosConfigurations.example;
  machine = modules: (example.extendModules { inherit modules; }).config;
  own = {
    burrow.github.apps.renovate = { id = 4; accountId = 5; slug = "sam-renovate"; keyFile = "/run/secrets/renovate-key"; };
    burrow.updates.renovate.app = "renovate";
  };
  base = example.config;
  apart = machine [ own ];
  failed = c: map (a: a.message) (lib.filter (a: !a.assertion) c.assertions);
  workersBot = "sam-workers[bot] <3+sam-workers[bot]@users.noreply.github.com>";
  renovateBot = "sam-renovate[bot] <5+sam-renovate[bot]@users.noreply.github.com>";
  wrongBot = failed (machine [ own { burrow.updates.renovate.gitAuthor = workersBot; } ]);
  # The address is what Renovate and the steward compare; the name is free.
  renamed = failed (machine [ own { burrow.updates.renovate.gitAuthor = "Renovate <5+sam-renovate[bot]@users.noreply.github.com>"; } ]);
  # Renovate's settings can't name another author than gitAuthor, the one
  # the steward knows its commits by: not another bot, not none, and not
  # beside an App or without one. The same one is no conflict.
  inSettings = author: failed (machine [ own { burrow.updates.renovate.settings.gitAuthor = author; } ]);
  settingsForeign = inSettings workersBot;
  settingsNull = inSettings null;
  settingsSame = inSettings renovateBot;
  userSettings = failed (machine [ {
    burrow.github.machinery = lib.mkForce null;
    burrow.updates.renovate.gitAuthor = "Renovate <renovate@example.org>";
    burrow.updates.renovate.settings.gitAuthor = "Other <other@example.org>";
  } ]);
  unknown = failed (machine [ { burrow.updates.renovate.app = "nope"; } ]);
  # As the user while the machinery is an App: renovate-sweep couldn't tell
  # Renovate's comments.
  asUser = failed (machine [ { burrow.updates.renovate.app = null; } ]);
  # Each machine's Renovate, its sweep and its steward, as the check reads them.
  parts = c: {
    run = c.systemd.services.renovate.serviceConfig.ExecStart;
    sweep = "${lib.head (lib.splitString " " c.systemd.services.renovate-sweep.serviceConfig.ExecStart)}";
    settings = c.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE;
    unit = c.systemd.units."renovate.service".text;
    steward = c.environment.etc."burrow/autoland.json".source;
  };
  evals = {
    defaultIsMachinery = base.burrow.updates.renovate.app == "workers";
    baseAuthor = base.burrow.updates.renovate.gitAuthor == workersBot;
    apartAuthor = apart.burrow.updates.renovate.gitAuthor == renovateBot;
    baseClean = failed base == [ ];
    apartClean = failed apart == [ ];
    renamedClean = renamed == [ ];
    wrongBotFails = lib.length wrongBot == 1 && lib.hasInfix "gitAuthor" (lib.head wrongBot) && lib.hasInfix renovateBot (lib.head wrongBot);
    settingsForeignFails = lib.any (lib.hasInfix "settings.gitAuthor is \"${workersBot}\"") settingsForeign;
    settingsNullFails = lib.any (lib.hasInfix "settings.gitAuthor is null") settingsNull;
    settingsSameClean = settingsSame == [ ];
    userSettingsFails = lib.any (lib.hasInfix "settings.gitAuthor is \"Other <other@example.org>\"") userSettings;
    unknownFails = lib.any (lib.hasInfix "renovate.app is nope") unknown;
    asUserFails = lib.any (lib.hasInfix "Renovate acts as the user") asUser;
  };
  wrong = lib.attrNames (lib.filterAttrs (_: ok: !ok) evals);
  b = parts base;
  a = parts apart;
  apps = base.burrow.github.apps;
  rapps = apart.burrow.github.apps;
in pkgs.runCommand "renovate-app-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail 'renovate.app: ${w} is wrong'\n") wrong}
  # The config a wrapper has built in, by its store path in the binary.
  built() { grep -aoE "/nix/store/[a-z0-9]{32}-$2\.json" "$1" | head -1; }
  brun=$(built ${b.run} renovate-run); arun=$(built ${a.run} renovate-run)
  bsweep=$(built ${b.sweep} renovate-sweep); asweep=$(built ${a.sweep} renovate-sweep)
  [ -n "$brun" ] && [ -n "$arun" ] && [ -n "$bsweep" ] && [ -n "$asweep" ] || fail "no config found in the wrappers"

  # Unset: renovate-run acts as the machinery's App, its gh that App's.
  jq -e --argjson env '${builtins.toJSON apps.workers.env}' --arg gh '${apps.workers.gh}/bin/gh' \
    '.app.env == $env and .app.owners == ["sam"] and .tools.gh == $gh' "$brun" >/dev/null \
    || fail "unset, renovate-run isn't the machinery's: $(jq -c '{app, gh: .tools.gh}' "$brun")"
  jq -e --arg a '${workersBot}' '.gitAuthor == $a' ${b.settings} >/dev/null || fail "unset, Renovate's gitAuthor isn't the machinery's bot"
  jq -e '.app == {name: "workers", bot: "sam-workers"} and .renovate == "sam-workers[bot]"' "$bsweep" >/dev/null \
    || fail "unset, renovate-sweep: $(jq -c '{app, renovate}' "$bsweep")"
  jq -e '.renovate == "sam-workers[bot]" and .renovateEmail == "3+sam-workers[bot]@users.noreply.github.com"
    and .bots == ["dependabot", "renovate", "sam-workers"]' ${b.steward} >/dev/null \
    || fail "unset, the steward's Renovate: $(jq -c '{renovate, renovateEmail, bots}' ${b.steward})"

  # Its own App: renovate-run acts as it, and the machinery's App is in
  # nothing Renovate runs.
  jq -e --argjson env '${builtins.toJSON rapps.renovate.env}' --arg gh '${rapps.renovate.gh}/bin/gh' \
    '.app.env == $env and .tools.gh == $gh' "$arun" >/dev/null \
    || fail "renovate-run doesn't act as its own App: $(jq -c '{app, gh: .tools.gh}' "$arun")"
  jq -e --arg a '${renovateBot}' '.gitAuthor == $a' ${a.settings} >/dev/null || fail "Renovate's gitAuthor isn't its own App's bot"
  for f in "$arun" ${a.settings} ${pkgs.writeText "renovate.service" a.unit}; do
    ! grep -qE 'gh-token/workers|${lib.escapeRegex "${rapps.workers.gh}"}|sam-workers' "$f" || fail "$f names the machinery's App"
  done
  # The sweep's jobs are still the machinery's; Renovate's word is the new bot's.
  jq -e '.app == {name: "workers", bot: "sam-workers"} and .renovate == "sam-renovate[bot]"' "$asweep" >/dev/null \
    || fail "renovate-sweep with Renovate's own App: $(jq -c '{app, renovate}' "$asweep")"
  # The steward knows Renovate by its own bot, and still trusts the machinery's.
  jq -e '.renovate == "sam-renovate[bot]" and .renovateEmail == "5+sam-renovate[bot]@users.noreply.github.com"
    and .bots == ["dependabot", "renovate", "sam-workers", "sam-renovate"] and .machineryLogin == "sam-workers[bot]"' ${a.steward} >/dev/null \
    || fail "the steward's Renovate: $(jq -c '{renovate, renovateEmail, bots, machineryLogin}' ${a.steward})"
  upkeep=$(jq -r '.upkeep[1]' ${a.steward})
  jq -e '.renovate == "sam-renovate[bot]" and (.bots | index("sam-renovate"))' "$upkeep" >/dev/null \
    || fail "burrow-upkeep's Renovate: $(jq -c '{renovate, bots}' "$upkeep")"
  touch $out
''
