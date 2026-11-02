# The checks of Renovate's setup, each as Renovate's own code reads it: what its
# runs cover, what its config decides, and what the sweep does with its PRs.
{ self, pkgs }:
{
  # Which repos a run of Renovate on one repo covers: the ones its
  # autodiscoverFilter takes, as Renovate's own code has it.
  renovate-covers = pkgs.runCommand "renovate-covers-check" { } ''
    # A /regex/ minds case: Sam/Secret is out, sam/secret isn't (so
    # renovate@ asks with GitHub's spelling).
    covers() { ${pkgs.lib.getExe pkgs.nodejs_24} ${../modules/updates/renovate-covers.mjs} ${pkgs.renovate} '["sam/*", "Kim/r", "!sam/{old,archive}", "!/^sam\/tmp-/", "!/^Sam\/Secret$/"]' "$1"; }
    for r in sam/r SAM/X kim/r sam/secret; do covers "$r" || { echo "$r: not covered"; exit 1; }; done
    for r in sam/old sam/archive sam/tmp-1 Sam/Secret kim/q other/r; do
      rc=0; covers "$r" || rc=$?
      [ "$rc" = 10 ] || { echo "$r: $rc, not 10"; exit 1; }
    done
    touch $out
  '';
  # Which Renovate PRs renovate-sweep hands over and lists, with stand-ins for gh and proj.
  renovate-sweep = pkgs.runCommand "renovate-sweep-check" { nativeBuildInputs = [ pkgs.python3 pkgs.git pkgs.jq pkgs.gnused ]; } ''
    python3 -I ${./renovate-sweep.py} ${../modules/updates/renovate-sweep.sh} ${../modules/updates/major-seal.py} ${../modules/autoland/seal.py} ${../modules/autoland/lock_wait.py}
    touch $out
  '';
  # Which repos Renovate's runs cover, with whose token: gh's login for
  # all of them, or a run each with its owner's App token.
  renovate-run = pkgs.runCommand "renovate-run-check" { nativeBuildInputs = [ pkgs.python3 pkgs.util-linux ]; } ''
    python3 -I ${./renovate-run.py} ${../modules/updates/renovate.sh}
    touch $out
  '';
  # What decides a lock file refresh's release age in a Renovate config,
  # as Renovate itself migrates it, resolves its presets (at any scope)
  # and reads its durations; what it can't resolve or read fails.
  release-ages = pkgs.runCommand "release-ages-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
    export HOME=$TMPDIR
    read() { echo "$1" | ${pkgs.lib.getExe pkgs.nodejs_24} ${../modules/updates/release-ages.mjs} ${pkgs.renovate} 2>/dev/null; }
    ages() { read "$1" | jq -c .ages; }
    test "$(ages '{"extends": ["config:best-practices"]}')" = '[259200000]'
    test "$(ages '{"packageRules": [{"matchManagers": ["cargo"], "extends": ["security:minimumReleaseAgeNpm"]}]}')" = '[259200000]'
    test "$(ages '{"cargo": {"extends": ["security:minimumReleaseAgeNpm"]}}')" = '[259200000]'
    test "$(ages '{"cargo": {"minimumReleaseAge": "1 week 2 days"}, "minimumReleaseAge": null, "statusCheckNames": {"minimumReleaseAge": "x"}, "statusCheckWhen": {"minimumReleaseAge": "always"}}')" = '[777600000]'
    test "$(ages '{"extends": ["config:recommended", "helpers:pinGitHubActionDigests"]}')" = '[]'
    # Old settings Renovate migrates, at any scope.
    test "$(ages '{"stabilityDays": 14, "packageRules": [{"matchManagers": ["cargo"], "stabilityDays": 10}]}')" = '[1209600000,864000000]'
    # Where else policy comes from.
    test "$(read '{"configFileNames": ["ci/renovate config.json"]}' | jq -c '[.configFileNames, .inheritConfig]')" = '[["ci/renovate config.json"],false]'
    test "$(read '{}' | jq -c '[has("configFileNames"), .inheritConfig]')" = '[false,false]'
    test "$(read '{"configFileNames": null}' | jq -c '[has("configFileNames"), .configFileNames]')" = '[true,null]'
    test "$(read '{"force": {"inheritConfig": true}}' | jq -c .inheritConfig)" = true
    test "$(read '{"forkToken": "t"}' | jq -c .forkMode)" = true
    test "$(read '{"persistRepoData": true}' | jq -c .forkMode)" = false
    for bad in '{"extends": ["github>someone/presets"]}' '{"extends": ["config:nope"]}' '{"minimumReleaseAge": "soon"}' '{"minimumReleaseAge": 5}' \
               '{"repositories": [{"repository": "x/y", "configFileNames": ["c.json"]}]}'; do
      if read "$bad"; then echo "took $bad"; exit 1; fi
    done
    touch $out
  '';
  # The example's Renovate config, as Renovate's own rules read it: a
  # pin on a GitHub repo of its trusted owner's (sam, any case) waits
  # for no soak; anyone else's still does, and so does a sam/... on
  # another host or a registry's package that claims a sam repo.
  renovate-soak = pkgs.runCommand "renovate-soak-check" { } ''
    export HOME=$TMPDIR
    got=$(${pkgs.lib.getExe pkgs.nodejs_24} ${./renovate-soak.mjs} ${pkgs.renovate} \
      ${self.nixosConfigurations.example.config.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE} \
      '{"datasource":"github-digest","packageName":"sam/homelab"}' \
      '{"datasource":"github-tags","packageName":"Sam/Tools"}' \
      '{"datasource":"git-refs","packageName":"https://github.com/sam/homelab.git","manager":"nix","sourceUrl":"https://github.com/sam/homelab"}' \
      '{"datasource":"github-digest","packageName":"actions/checkout"}' \
      '{"datasource":"github-tags","packageName":"samuel/x"}' \
      '{"datasource":"github-tags","packageName":"sam/tools","registryUrl":"https://ghe.example.com"}' \
      '{"datasource":"docker","packageName":"sam/tools","sourceUrl":"https://github.com/sam/tools"}' \
      '{"datasource":"npm","packageName":"homelab","manager":"npm","sourceUrl":"https://github.com/sam/homelab"}' \
      2>/dev/null)
    want='github-digest sam/homelab none
    github-tags Sam/Tools none
    git-refs https://github.com/sam/homelab.git none
    github-digest actions/checkout 3 days
    github-tags samuel/x 3 days
    github-tags sam/tools at https://ghe.example.com 3 days
    docker sam/tools 3 days
    npm homelab 3 days'
    [ "$got" = "$(sed 's/^ *//' <<<"$want")" ] || { echo "got: $got"; exit 1; }
    touch $out
  '';
  # Which updates Renovate merges itself under the example's config, as
  # Renovate's own rules read it: all but a major, a commit pin of an
  # action (pinDigest) included. The steward leaves Renovate's own PRs
  # to Renovate, so one it won't merge sits green for good. In a repo
  # with its train on it merges none: the train lands them all.
  renovate-automerge = pkgs.runCommand "renovate-automerge-check" { } ''
    export HOME=$TMPDIR
    check() {
      ${pkgs.lib.getExe pkgs.nodejs_24} ${./renovate-automerge.mjs} ${pkgs.renovate} \
        ${self.nixosConfigurations.example.config.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE} "$1" 2>/dev/null
    }
    train=$(check sam/homelab)
    [ -z "$(grep -v ' no$' <<<"$train")" ] && [ "$(wc -l <<<"$train")" = 7 ] || { echo "in the train repo: $train"; exit 1; }
    got=$(check sam/other)
    want='major no
    minor yes
    patch yes
    pin yes
    pinDigest yes
    digest yes
    lockFileMaintenance yes'
    [ "$got" = "$(sed 's/^ *//' <<<"$want")" ] || { echo "got: $got"; exit 1; }
    touch $out
  '';
  # A PR in its repo's train has the steward's train label on, and that's
  # the label Renovate pushes nothing to a PR under: a new head on one
  # throws away the candidate it rides in, gate and all.
  renovate-train-label = let
    example = self.nixosConfigurations.example.config;
  in pkgs.runCommand "renovate-train-label-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
    got=$(jq -r .stopUpdatingLabel ${example.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE})
    [ "$got" = ${pkgs.lib.escapeShellArg example.burrow.autoland.labels.waiting.train} ] || { echo "stopUpdatingLabel: $got"; exit 1; }
    touch $out
  '';
  # The PR titles Renovate gives the example's updates, as Renovate's
  # own code writes them: each the house style's `area: plain
  # sentence`, "deps: ...", whichever type config:recommended would
  # give it. The title is the subject the steward squashes onto main.
  renovate-titles = pkgs.runCommand "renovate-titles-check" { } ''
    export HOME=$TMPDIR
    got=$(${pkgs.lib.getExe pkgs.nodejs_24} ${./renovate-titles.mjs} ${pkgs.renovate} \
      ${self.nixosConfigurations.example.config.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE} 2>/dev/null)
    want='deps: update dependency left-pad to 1.3.1
    deps: update dependency vitest to 3.2.0
    deps: update dependency vite to v7
    deps: lock file maintenance (npm)
    deps: update dependency net.fabricmc:fabric-loader to 0.20.1
    deps: lock file maintenance (gradle)
    deps: update rust crate tokio to 1.41.0
    deps: lock file maintenance (cargo)
    deps: update fieldmouseworks/burrow digest to 0fce5b8
    deps: lock file maintenance (nix)'
    [ "$got" = "$(sed 's/^ *//' <<<"$want")" ] || { echo "got: $got"; exit 1; }
    touch $out
  '';
  # What a dashboard's Repository Problems lists under the example's
  # config, by Renovate's own logger: not the refresh it was told to take
  # without a release timestamp, but still a warning that's a problem.
  # A lock file refresh that changes nothing commits nothing, by Renovate's own
  # cargo manager (modules/updates/renovate-package.nix).
  renovate-cargo-unchanged = pkgs.runCommand "renovate-cargo-unchanged-check" { } ''
    export HOME=$TMPDIR
    LOG_LEVEL=fatal ${pkgs.lib.getExe pkgs.nodejs_24} ${./renovate-cargo-unchanged.mjs} ${import ../modules/updates/renovate-package.nix pkgs}
    touch $out
  '';
  renovate-problems = pkgs.runCommand "renovate-problems-check" { } ''
    export HOME=$TMPDIR
    got=$(LOG_LEVEL=fatal ${pkgs.lib.getExe pkgs.nodejs_24} ${./renovate-problems.mjs} ${pkgs.renovate} \
      ${self.nixosConfigurations.example.config.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE} 2>/dev/null)
    [ "$got" = artifactErrors ] || { echo "got: $got"; exit 1; }
    touch $out
  '';
}
