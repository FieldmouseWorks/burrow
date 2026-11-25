# renovate-sweep's seal key in conaryd's flow (autoland/shadow.nix,
# burrow#1316), for real: with the sweep on, the flow config names the key
# by its file (`major_key_file`) and holds no key, and the controller,
# loading it with Conary's own loader as conaryd does at each pass, gets
# the sweep's verdict.key. The sweep makes that key as it runs, so the
# keys unit has it made first (renovate-sweep-key), and a key the sweep
# makes anew (one that isn't a key, or none at all) reaches the flow by
# the path unit on it. burrow.user, whom the sweep runs as, holds
# verdict.key itself; the flow's copy is the controller's alone. No store
# path holds the key.
#
#   nix build .#checks.x86_64-linux.autoland-major-key -L
{ self, pkgs }:
let
  cn = pkgs.writeShellScriptBin "cn" "exit 0";
  repo = "acme/widget";
  flowFiles = "${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-flow-files}/bin/burrow-flow-files";
in
pkgs.testers.runNixOSTest {
  name = "autoland-major-key";
  # burrow.updates turns on burrow.switch, whose module patches
  # switch-to-configuration with an overlay.
  node.pkgsReadOnly = false;

  nodes.machine = { lib, ... }: {
    imports = [
      self.nixosModules.autoland self.nixosModules.ledger self.nixosModules.github self.nixosModules.updates
      ((import ../examples/machine.nix { inherit lib; }) // { nixpkgs = { }; fileSystems = { }; boot = { }; })
    ];
    config = {
      burrow.claude.package = pkgs.hello;
      burrow.ledger = {
        enable = true;
        package = cn;
        owners = [ "acme" ];
        anchorRepo = repo;
        reviewers = [ "acme-steward[bot]" ];
        controllerUser = "conary-flow";
      };
      burrow.events = {
        host = "example.org";
        secretFile = "/etc/webhook";
      };
      environment.etc.webhook = { text = "stand-in"; mode = "0400"; };
      burrow.github = {
        apps.workers = {
          id = 7; accountId = 8;
          slug = "acme-workers";
          keyFile = "/run/keys/workers.pem";
          api = "http://127.0.0.1:8080";
        };
        machinery = "workers";
      };
      burrow.autoland = {
        enable = true;
        owners = [ "acme" ];
        authors = [ "acme" ];
        app = { id = 1; accountId = 2; slug = "acme-steward"; keyFile = "/run/keys/steward.pem"; };
        shadow = [ repo ];
      };
      burrow.landing.conaryd = [ repo ];
      burrow.settings.allowed = [ repo ];
      burrow.updates = {
        enable = true;
        origin = "https://github.com/acme/widget";
        renovate = {
          enable = true;
          autodiscoverFilter = [ "acme/*" ];
        };
      };
      environment.systemPackages = [ pkgs.jq ];
      # No GitHub here: a pass would fail.
      burrow.events.enable = lib.mkForce false;
    };
  };

  testScript = { nodes, ... }: let
    config = "${nodes.machine.burrow.autoland.flowConfigs}/${repo}.json";
    key = "${nodes.machine.burrow.stateDir}/renovate-sweep/verdict.key";
    copy = "/run/autoland-flow-keys/major-seal";
    user = nodes.machine.burrow.user;
  in ''
    machine.wait_for_unit("multi-user.target")

    def loads_the_sweeps_key():
        # The sweep's key as the controller may read it here, to compare.
        machine.succeed("install -m 0400 -o conary-flow ${key} /run/key-copy")
        try:
            machine.succeed("runuser -u conary-flow -- ${flowFiles} key major ${config} /run/key-copy")
        finally:
            machine.succeed("rm -f /run/key-copy")

    with subtest("the sweep's key is made before the flow's keys unit takes it"):
        machine.wait_until_succeeds("systemctl is-active autoland-flow-keys.service", timeout=60)
        # The sweep's own: its user's, and no one else's to read.
        assert machine.succeed("stat -c '%U %a' ${key}").strip() == "${user} 600"
        machine.succeed("runuser -u conary-flow -- test ! -r ${key}")

    with subtest("the config names the key's file and holds no key"):
        machine.wait_until_succeeds("test -e ${config}", timeout=60)
        machine.succeed("jq -e '.settings.major_key_file == \"${copy}\" and (.settings | has(\"major_key\") | not)' ${config}")
        # conaryd's is the same file: its repo's entry names it.
        machine.succeed("jq -e '.flow_config[\"${repo}\"] == \"${config}\"' ${nodes.machine.burrow.landing.eventsTemplate}")

    with subtest("the controller loads the config's major_key, and it is the sweep's"):
        machine.succeed("runuser -u conary-flow -- ${flowFiles} flow ${config}")
        loads_the_sweeps_key()
        machine.succeed("printf '%064d' 0 > /run/key-other && chown conary-flow /run/key-other")
        machine.fail("runuser -u conary-flow -- ${flowFiles} key major ${config} /run/key-other")
        machine.succeed("rm /run/key-other")

    with subtest("burrow.user reads not the flow's copy"):
        machine.fail("runuser -u ${user} -- cat ${copy}")
        machine.fail("runuser -u ${user} -- ${flowFiles} flow ${config}")

    with subtest("both units that run the flow require the keys' unit"):
        for unit in ["conary-flow.service", "autoland-shadow@acme-widget.service"]:
            requires = machine.succeed(f"systemctl show -P Requires {unit}").split()
            assert "autoland-flow-keys.service" in requires, (unit, requires)

    keys = [machine.succeed("cat ${key}").strip()]

    with subtest("a key the sweep makes anew reaches the flow"):
        # One that isn't a key, which the sweep's next seal replaces (as
        # its --key does).
        machine.succeed("runuser -u ${user} -- sh -c 'echo short > ${key}'")
        machine.succeed("systemctl start renovate-sweep-key.service")
        keys.append(machine.succeed("cat ${key}").strip())
        assert keys[-1] != keys[0] and len(keys[-1]) == 64, keys
        machine.wait_until_succeeds("install -m 0400 -o conary-flow ${key} /run/key-copy && runuser -u conary-flow -- ${flowFiles} key major ${config} /run/key-copy", timeout=60)
        machine.succeed("rm -f /run/key-copy")
        # None at all: the keys unit, taking it again, has it made again.
        machine.succeed("rm ${key}")
        machine.wait_until_succeeds("test -s ${key}", timeout=60)
        keys.append(machine.succeed("cat ${key}").strip())
        assert keys[-1] not in keys[:-1], keys
        machine.wait_until_succeeds("install -m 0400 -o conary-flow ${key} /run/key-copy && runuser -u conary-flow -- ${flowFiles} key major ${config} /run/key-copy", timeout=60)
        machine.succeed("rm -f /run/key-copy")
        machine.succeed("systemctl is-active autoland-flow-keys.service")

    with subtest("no store path of the machine holds the key"):
        # Its whole closure: the rendered configs and units included.
        # grep exits 1 finding nothing, and 2 on an error.
        patterns = " ".join(f"-e {k}" for k in keys)
        held = machine.succeed(f"grep -rlsF {patterns} $(nix-store -qR /run/current-system) || [ $? = 1 ]", timeout=1800)
        assert held.strip() == "", held
        # As a check that grep finds the key where it is.
        machine.succeed(f"grep -qF -- {keys[-1]} ${key}")
  '';
}
