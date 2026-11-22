# The flow config's plugins (autoland/shadow.nix): each enabled plugin's
# executable in `plugins`, its choice in `execution` (a plugin's own in the
# base the build renders, a role's only as the machine renders it from the
# live role settings, burrow#1393: a build's names no model, which every
# job refuses), and, where a
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
  # The stuck role's job plugin beside the fix plugin. Conary's flow config
  # names no `job` plugin until FP11 (Conary#422), so its loader refuses
  # the file, as below, and so would a machine's build with it on.
  jobbed = machine {
    imports = [ controlled ];
    burrow.autoland.plugins.fix.enable = true;
    burrow.autoland.plugins.job.enable = true;
  };
  entries = m: m.burrow.autoland.flow.base.entries;
  # The manifest the machine renders its live configs by.
  manifest = m: lib.last (lib.splitString " " m.systemd.services.autoland-flow-configs.serviceConfig.ExecStart);
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
    # (tests/autoland-flow-keys.nix runs it). Either way they require the
    # flow's state, carried to its user (tests/ledger-flow-data.nix).
    offNoKeys = lib.all (m: !(m.systemd.services ? autoland-flow-keys)
      && m.systemd.services."autoland-shadow@".requires == [ "burrow-flow-data.service" ]
      && m.burrow.autoland.flow.needs == [ "burrow-flow-data.service" ]) [ off on plain ];
    judgedKeys = let k = judged.systemd.services.autoland-flow-keys; in
      k.serviceConfig.User == "conary-flow"
      && k.serviceConfig.LoadCredential == [ "judge-seal:/run/secrets/judge-seal" ]
      && judged.systemd.services."autoland-shadow@".requires == [ "burrow-flow-data.service" "autoland-flow-keys.service" ]
      && judged.burrow.autoland.flow.needs == [ "burrow-flow-data.service" "autoland-flow-keys.service" ];
    # Named for `job` only when it's on.
    jobNamed = let p = (entries jobbed)."sam/homelab.json".plugins; in
      lib.attrNames p == [ "fix" "job" ] && p.job == "${jobbed.burrow.autoland.plugins.job.plugin}";
    jobUnnamed = !((entries on)."sam/homelab.json".plugins ? job);
    # The live configs are where every reader reads them, and are rendered
    # at boot, each switch and each change to the role settings.
    liveRead = on.burrow.autoland.flowConfigs == "/var/lib/burrow/flow-configs"
      && jobbed.burrow.autoland.plugins.fix.config.flow == jobbed.burrow.autoland.flowConfigs
      && on.burrow.autoland.plugins.proof.config.flow == on.burrow.autoland.flowConfigs
      && judged.burrow.autoland.plugins.judge.config.flow == judged.burrow.autoland.flowConfigs;
    rendered = let r = on.systemd; in
      r.paths.autoland-flow-configs.pathConfig.PathChanged == [ "/var/lib/burrow/settings/overlay.roles.json" ]
      && lib.elem "multi-user.target" r.services.autoland-flow-configs.wantedBy
      && r.services.autoland-flow-configs.serviceConfig.User == "autoland-flow-configs"
      && lib.elem "autoland-flow-configs.service" r.services."autoland-shadow@".after
      && !(lib.elem "autoland-flow-configs.service" r.services."autoland-shadow@".wants)
      && lib.elem "autoland-flow-configs.service" r.services.burrow-fix.after
      && lib.elem "autoland-flow-configs.service" r.services.autoland-proof.after
      && lib.elem "autoland-flow-configs.service" judged.systemd.services.autoland-judge.after;
    # With no shadowed repo, nothing to render and no one to read it.
    noRenderWithoutShadow = let bare = self.nixosConfigurations.example.config; in
      bare.burrow.autoland.shadow == [ ] && !(bare.systemd.services ? autoland-flow-configs)
      && !(bare.systemd.paths ? autoland-flow-configs) && bare.burrow.autoland.flow.render == [ ];
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
  ${plugins.proof.choice} > proof.json
  jq -e '.engine == "burrow-proof" and .model == null and .effort == null' proof.json >/dev/null \
    || fail "the proof plugin's choice isn't its own engine's: $(cat proof.json)"
  # The base holds the proof plugin's own choice, and no role's: a build
  # reads no live settings.
  jq -e --slurpfile proof proof.json '.execution == {proof: $proof[0]}' $homelab >/dev/null \
    || fail "the base's execution isn't the proof plugin's own choice alone: $(jq -c .execution $homelab)"
  jq -e '.execution | has("judge") | not' ${file on "sam/plain"} >/dev/null || fail "a plugin that's off has a choice"

  # The machine's render, here on role settings of its own: the manifest's
  # base and roles, its settings source and output moved into the sandbox.
  manifest=${manifest on}
  jq -e '.roles == {fix: "${plugins.fix.role}"} and .out == "/var/lib/burrow/flow-configs"' $manifest >/dev/null \
    || fail "the render's roles aren't the role-run plugins': $(cat $manifest)"
  mkdir -p settings
  echo "{\"overlay\": \"$PWD/settings/overlay.json\"}" > settings.json
  jq --arg s "$PWD/settings.json" --arg o "$PWD/live" '.settings = $s | .out = $o' $manifest > render.json
  settings() { ${self.packages.${system}.burrow-settings}/bin/burrow-settings "$PWD/settings.json" "$@" >/dev/null; }
  # The bug: with no model set live, the fixing role's choice names none,
  # which every fix job refuses, so the render refuses it, writing nothing.
  if ${flowFiles} render render.json 2>render.err; then
    fail "a render with no live model for the fixing role succeeded: $(cat live/sam/homelab.json)"
  fi
  grep -q "names no model" render.err || fail "the render was refused for something else: $(cat render.err)"
  [ ! -e live ] || [ -z "$(ls -A live)" ] || fail "a refused render wrote: $(ls -R live)"
  settings offer claude models stand-in-one stand-in-two
  settings model ${plugins.fix.role} claude stand-in-one high
  ${flowFiles} render render.json || fail "the render refused a role set live"
  jq -e --slurpfile proof proof.json \
    '.execution == {fix: {engine: "claude", model: "stand-in-one", effort: "high"}, proof: $proof[0]}' live/sam/homelab.json >/dev/null \
    || fail "the live execution isn't the role's as set: $(jq -c .execution live/sam/homelab.json)"
  for f in live/sam/*.json; do
    cmp <(jq -S 'del(.execution)' $f) <(jq -S 'del(.execution)' ${on.burrow.autoland.flow.base}/sam/$(basename $f)) \
      || fail "the render changed $f beyond its execution"
  done
  ${flowFiles} flow live/sam/homelab.json || fail "Conary's flow::Config refuses the rendered config"
  # A change to the role, rendered again: the next read is the new choice.
  settings model ${plugins.fix.role} claude stand-in-two
  ${flowFiles} render render.json || fail "the render refused a changed role"
  jq -e '.execution.fix == {engine: "claude", model: "stand-in-two", effort: null}' live/sam/homelab.json >/dev/null \
    || fail "a changed role isn't rendered: $(jq -c .execution live/sam/homelab.json)"
  producers=$(jq -r .evidence.producers $homelab)
  jq -e --arg key ${keys.proof} '. == [{name: "sam/proof", key: $key, checks: ["gate", "vm"]}]' "$producers" >/dev/null \
    || fail "the producers file isn't the proof producer's, trusted for homelab's proofs: $(cat "$producers")"
  ${flowFiles} producers "$producers" || fail "Conary's loader refuses the rendered producers file"
  ${flowFiles} flow $homelab ${file on "sam/plain"} ${file on "sam/bare"} || fail "Conary's flow::Config refuses a config with plugins"
  [ -e ${on.burrow.autoland.flow.base}/sam/bare.json ] || fail "the machine's configs don't all build"

  # With the job plugin on: named for `job`, and rendered on the stuck
  # role's choice.
  jobbed=${file jobbed "sam/homelab"}
  jq -e '.roles == {fix: "${jobbed.burrow.autoland.plugins.fix.role}", job: "${jobbed.burrow.autoland.plugins.job.role}"}' ${manifest jobbed} >/dev/null \
    || fail "the job kind isn't rendered on the stuck role: $(cat ${manifest jobbed})"
  jq -e 'has("execution") | not' $jobbed >/dev/null || fail "a base holds a role's choice: $(jq -c .execution $jobbed)"
  # Until Conary's flow config takes `plugins.job` (FP11), its loader
  # refuses the file, so a machine with the job plugin on fails its build
  # rather than every pass once live.
  if ${flowFiles} flow $jobbed; then
    fail "Conary's flow::Config now takes plugins.job: check here that it loads instead (SJ, FP11)"
  fi
  jq -c 'del(.plugins.job)' $jobbed > unjobbed.json
  ${flowFiles} flow unjobbed.json || fail "the flow config with the job plugin on is refused for more than its job key"

  # A producer trusted for no check fails the build that renders it.
  grep -q "names no check" ${untrusted}/testBuildFailure.log \
    || fail "a producer trusted for no check wasn't refused for it: $(cat ${untrusted}/testBuildFailure.log)"
  touch $out
''
