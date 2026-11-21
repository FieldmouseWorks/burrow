# The flow config's plugins (autoland/shadow.nix): each enabled plugin's
# executable in `plugins`, its role's choice in `execution`, and, where a
# repo has evidence, its producer in the producers file
# `evidence.producers` names, with its unpadded base64url public key and
# trusted for its own checks alone: the proofs' names, or the judge's
# judgment. Conary's own loaders read each config and producers file
# (crates/burrow-flow-files), as the machine's build does. The judge's
# rendering is shown on its producers alone, which need no build of it;
# the proof and fix plugins are rendered
# whole. With every plugin off, a config is byte for byte what it was before
# plugins were rendered (autoland-shadow-homelab.json). A producer with no
# check in a repo (the proof producer, where it has evidence and no proofs,
# as every train does yet) is left out of it, and the machine builds; one
# trusted for no check, put in a rendered producers file, fails its build
# on Conary's loader.
{ self, pkgs, nixpkgs, system, ... }:
let
  lib = nixpkgs.lib;
  keys = {
    proof = "-cJQ-PhqqsjCfRc-cjAfPbqv4JUSRtJnSPQUJMlij08";
    judge = "RBb1DHObOnTDtFA1uTBi9xeBGncjjA_H-miBBJmJKaI";
  };
  machine = plugins: (self.nixosConfigurations.example.extendModules {
    modules = [{
      burrow.autoland.shadow = [ "Sam/HomeLab" "sam/plain" "sam/bare" "sam/twin" ];
      # Evidence and proofs: its train off, as proofs need yet.
      burrow.autoland.repos."sam/homelab" = {
        train = false;
        trainNegative = "negative";
        proofRuns.gate = "make check";
        proofBuilds.vm = "checks.x86_64-linux.vm";
      };
      # Evidence, and no proofs.
      burrow.autoland.repos."sam/bare".trainNegative = "negative";
      # Bare's twin: the same settings, so the same flow config.
      burrow.autoland.repos."sam/twin".trainNegative = "negative";
      burrow.autoland.repos."sam/plain".proofRuns.gate = "make check";
    } plugins];
  }).config;
  controlled = { burrow.ledger.controllerUser = "conary-flow"; };
  proof = {
    enable = true;
    producer = "sam/proof";
    keyFile = "/run/secrets/proof-key";
    publicKey = keys.proof;
  };
  off = machine { };
  on = machine {
    imports = [ controlled ];
    burrow.autoland.plugins.fix.enable = true;
    burrow.autoland.plugins.proof = proof;
    # Two repos conaryd may have, with the same settings: the plugins add
    # nothing of a repo's own to its flow config.
    burrow.landing.conaryd = [ "sam/bare" "sam/twin" ];
  };
  # The judge beside the proof producer, evaluated, never built.
  judged = machine {
    imports = [ controlled ];
    burrow.autoland.plugins.proof = proof;
    burrow.autoland.plugins.judge = {
      enable = true;
      producer = "sam/judge";
      keyFile = "/run/secrets/judge-key";
      sealFile = "/run/secrets/judge-seal";
      claudeTokenFile = "/run/secrets/judge-claude-token";
      publicKey = keys.judge;
    };
  };
  entries = m: m.burrow.autoland.flowConfigs.entries;
  # One repo's config, built alone.
  file = m: repo: "${(entries m)."${repo}.json"}";
  # A repo's producers, as rendered, before Conary's loader reads them.
  producers = m: repo: (entries m)."${repo}.json".producers;
  # The machine with every plugin off and homelab as it was before plugins
  # were rendered (autoland-shadow-homelab.json).
  plain = (self.nixosConfigurations.example.extendModules {
    modules = [{
      burrow.autoland.shadow = [ "Sam/HomeLab" ];
      burrow.autoland.repos."sam/homelab".trainNegative = "negative";
    }];
  }).config;
  plugins = on.burrow.autoland.plugins;
  wired = {
    assertionsHold = lib.all (a: a.assertion) on.assertions;
    # The plugins read their configs where the machine keeps them, since
    # each config names the flow's configs, which name the plugins.
    proofEtc = let plain = builtins.unsafeDiscardStringContext; in
      lib.hasInfix "/bin/burrow-proof-plugin plugin /etc/burrow/autoland-proof-plugin.json" (plain plugins.proof.plugin.text)
      && plain "${on.environment.etc."burrow/autoland-proof-plugin.json".source}"
        == plain (lib.last (lib.splitString " " on.systemd.services.autoland-proof.serviceConfig.ExecStart));
    offNoProducers = producers off "sam/homelab" == null;
    plainNoProducers = producers on "sam/plain" == null;
    # Bare has evidence, and its proof producer no check there.
    bareNoProducers = producers on "sam/bare" == null;
    twins = file on "sam/bare" == file on "sam/twin";
    # The judge's seal key reaches the flow as a credential of a unit of
    # the controller's own, which the shadow's units require; with the
    # judge off there's no such unit and nothing requires one
    # (tests/autoland-flow-keys.nix runs it).
    offNoKeys = lib.all (m: !(m.systemd.services ? autoland-flow-keys)
      && m.systemd.services."autoland-shadow@".requires == [ ]
      && m.burrow.autoland.flow.needs == [ ]) [ off on plain ];
    judgedKeys = let k = judged.systemd.services.autoland-flow-keys; in
      k.serviceConfig.User == "conary-flow"
      && k.serviceConfig.LoadCredential == [ "judge-seal:/run/secrets/judge-seal" ]
      && judged.systemd.services."autoland-shadow@".requires == [ "autoland-flow-keys.service" ]
      && judged.burrow.autoland.flow.needs == [ "autoland-flow-keys.service" ];
    judgedProducers = builtins.fromJSON (producers judged "sam/homelab").producers == [
      { name = "sam/judge"; key = keys.judge; checks = [ "conary/judgment" ]; }
      { name = "sam/proof"; key = keys.proof; checks = [ "gate" "vm" ]; }
    ];
  };
  wrong = lib.attrNames (lib.filterAttrs (_: ok: !ok) wired);
  flowFiles = "${self.packages.${system}.burrow-flow-files}/bin/burrow-flow-files";
  # Homelab's producers file, as the machine builds it, with its producer
  # trusted for no check.
  untrusted = pkgs.testers.testBuildFailure ((producers on "sam/homelab").overrideAttrs {
    producers = builtins.toJSON [ { name = "sam/proof"; key = keys.proof; checks = [ ]; } ];
  });
in pkgs.runCommand "autoland-flow-plugins-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail 'the plugins are wired wrong: ${w}'\n") wrong}
  cmp ${file plain "sam/homelab"} ${./autoland-shadow-homelab.json} \
    || fail "with every plugin off, homelab's flow config isn't checks/autoland-shadow-homelab.json: $(cat ${file plain "sam/homelab"})
(a change to the flow's own settings updates that file with this, which it alone may: no plugin's key belongs there)"
  for f in ${file off "sam/homelab"} ${file off "sam/plain"}; do
    jq -e '.plugins == {} and (has("execution") | not) and (.evidence == null or (.evidence | has("producers") | not))' $f >/dev/null \
      || fail "$f names a plugin, a choice or a producer with every plugin off"
  done

  homelab=${file on "sam/homelab"}
  jq -e --arg fix ${plugins.fix.plugin} --arg proof ${plugins.proof.plugin} \
    '.plugins == {fix: $fix, proof: $proof}' $homelab >/dev/null \
    || fail "plugins aren't the enabled plugins' executables: $(jq -c .plugins $homelab)"
  ${plugins.fix.choice} > fix.json
  ${plugins.proof.choice} > proof.json
  jq -e '.engine == "burrow-proof" and .model == null and .effort == null' proof.json >/dev/null \
    || fail "the proof plugin's choice isn't its own engine's: $(cat proof.json)"
  jq -e --slurpfile fix fix.json --slurpfile proof proof.json \
    '.execution == {fix: $fix[0], proof: $proof[0]}' $homelab >/dev/null \
    || fail "execution isn't each enabled plugin's role's choice: $(jq -c .execution $homelab), not $(cat fix.json) $(cat proof.json)"
  jq -e '.execution | has("judge") | not' ${file on "sam/plain"} >/dev/null || fail "a plugin that's off has a choice"
  producers=$(jq -r .evidence.producers $homelab)
  jq -e --arg key ${keys.proof} '. == [{name: "sam/proof", key: $key, checks: ["gate", "vm"]}]' "$producers" >/dev/null \
    || fail "the producers file isn't the proof producer's, trusted for homelab's proofs: $(cat "$producers")"
  ${flowFiles} producers "$producers" || fail "Conary's loader refuses the rendered producers file"
  ${flowFiles} flow $homelab ${file on "sam/plain"} ${file on "sam/bare"} || fail "Conary's flow::Config refuses a config with plugins"
  [ -e ${on.burrow.autoland.flowConfigs}/sam/bare.json ] || fail "the machine's configs don't all build"

  # A producer trusted for no check fails the build that renders it.
  grep -q "names no check" ${untrusted}/testBuildFailure.log \
    || fail "a producer trusted for no check wasn't refused for it: $(cat ${untrusted}/testBuildFailure.log)"
  touch $out
''
