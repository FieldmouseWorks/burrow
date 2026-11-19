# The fix plugin and its broker (autoland/fix.nix): off by default, with no
# unit; on, a broker unit as burrow.user (proj's jobs are that user's)
# listening in its own directory under the producers', which only the
# flow's controller may enter, and a plugin wrapper the flow configuration
# names. With no controller it's refused: the broker would trust
# burrow.user, the user authoring jobs run as.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  machine = modules: (self.nixosConfigurations.example.extendModules { inherit modules; }).config;
  off = machine [ ];
  on = machine [{
    burrow.ledger.controllerUser = "conary-flow";
    burrow.autoland.plugins.fix.enable = true;
  }];
  uncontrolled = machine [{ burrow.autoland.plugins.fix.enable = true; }];
  failed = map (a: a.message) (lib.filter (a: !a.assertion) uncontrolled.assertions);
  broker = on.systemd.services.burrow-fix;
  plugin = on.burrow.autoland.plugins.fix.plugin;
  # The config the broker and the plugin read, and the file it's in, from
  # the broker's ExecStart.
  read = on.burrow.autoland.plugins.fix.config;
  configFile = builtins.unsafeDiscardStringContext (lib.last (lib.splitString " " broker.serviceConfig.ExecStart));
  plain = builtins.unsafeDiscardStringContext;
  wiring = {
    offByDefault = !(off.systemd.services ? burrow-fix);
    holds = lib.all (a: a.assertion) on.assertions;
    refusedWithoutController = lib.any (lib.hasInfix "plugins.fix: only the flow's controller may ask") failed;
    user = broker.serviceConfig.User == on.burrow.user;
    socketDir = broker.serviceConfig.RuntimeDirectory == "conary-producers/fix"
      && broker.serviceConfig.SupplementaryGroups == [ "conary-flow" ];
    broker = lib.hasSuffix "/bin/burrow-fix broker ${configFile}" (plain broker.serviceConfig.ExecStart);
    plugin = lib.hasInfix "/bin/burrow-fix plugin ${configFile}" (plain plugin.text);
    config = read.socket == "/run/conary-producers/fix/broker.sock" && read.controller == "conary-flow"
      && read.role == "fix" && read.jobs == on.burrow.proj.jobsDir && lib.hasSuffix "/bin/proj" (lib.head read.proj)
      && read.settings == on.burrow.settings.effective && read.state == "${on.burrow.stateDir}/fix";
  };
  wrong = lib.attrNames (lib.filterAttrs (_: ok: !ok) wiring);
in pkgs.runCommand "autoland-fix-check" { } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail 'the fix broker is wrong: ${w}'\n") wrong}
  touch $out
''
