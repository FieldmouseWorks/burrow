# The build store as its module runs it: nginx serves the cache and hands
# an upload to the socket-activated door, which asks cn (a stand-in here,
# admitting the token `public` as a public listed repo's run and refusing
# any other, as Conary's `cn attest job` does) and writes only NARs whose
# hash is the manifest's. A store that trusts only the machine's key then
# substitutes from it over HTTP, and nginx's fetch is what the cap reads.
# Conary's own tests hold `cn attest job` to the signer rule; the crate's
# hold the door to cn's answer and the cache to its cap.
#
# The store is one location on one of nginx's hosts: with its fetched/
# gone, nginx still starts and its other hosts still serve.
#
#   nix build .#checks.x86_64-linux.builds -L
{ self, pkgs }:
let
  cn = pkgs.writeShellScriptBin "cn" ''
    case "$1" in policy) exit 0 ;; attest) ;; *) exit 1 ;; esac
    case "$(cat)" in
      public) echo '{"repository":"sam/homelab","visibility":"public"}' ;;
      *) echo "cn: the job's token doesn't hold: its token expired" >&2; exit 65 ;;
    esac
  '';
  # Two small paths of the machine's own, with no references.
  one = pkgs.writeText "one" "built once\n";
  two = pkgs.writeText "two" "built twice\n";
  # One that refers to both.
  three = pkgs.writeText "three" "${two} ${one}\n";
in
pkgs.testers.runNixOSTest {
  name = "builds";

  nodes.machine = {
    imports = [ self.nixosModules.builds ];
    burrow = {
      builds = {
        enable = true;
        host = "localhost";
        repos = [ "sam/homelab" ];
      };
      ledger = {
        enable = true;
        package = cn;
        anchorRepo = "sam/homelab";
        reviewers = [ "sams-steward[bot]" ];
      };
      github = {
        apps.workers = { id = 2; accountId = 3; slug = "sam-workers"; keyFile = "/run/secrets/workers-key"; };
        machinery = "workers";
      };
    };
    # burrow.user's default, whose group the ledger's directories take.
    users.users.dev.isNormalUser = true;
    nix.settings.experimental-features = [ "nix-command" ];
    environment.systemPackages = [ pkgs.curl pkgs.jq ];
    system.extraDependencies = [ one two three ];
    # Another site on the same nginx, which the store mustn't take down.
    services.nginx.virtualHosts."other.test".locations."/".return = "200 'other site'";
  };

  testScript = ''
    import json

    def manifest(path):
        q = lambda f: machine.succeed(f"nix-store -q --{f} {path}").strip()
        # In whatever order a run lists them: Nix signs them sorted.
        return {"path": path, "narHash": q("hash"), "narSize": int(q("size")),
                "references": sorted(q("references").split(), reverse=True)}

    def upload(token, paths, nars=None):
        """POSTs paths' manifest and NARs (or `nars`, a file): the status."""
        m = json.dumps({"paths": [manifest(p) for p in paths]})
        machine.succeed(f"printf '%s\\n' '{m}' >/tmp/body")
        if nars:
            machine.succeed(f"cat {nars} >>/tmp/body")
        for p in [] if nars else paths:
            machine.succeed(f"nix-store --dump {p} >>/tmp/body")
        return machine.succeed(
            f"curl -sS -o /tmp/answer -w '%{{http_code}}' -H 'Authorization: Bearer {token}' "
            "--data-binary @/tmp/body http://localhost/builds/upload"
        )

    # Nix remembers a missing narinfo for an hour: a path refused first
    # would read as missing when it's later admitted.
    fresh = "--option narinfo-cache-negative-ttl 0"

    def substitute(path, root):
        key = machine.succeed("curl -fsS http://localhost/builds/public-key").strip()
        return machine.execute(
            f"nix copy --from http://localhost/builds --to 'local?root={root}' {path} "
            f"--option trusted-public-keys '{key}' --option substituters ''' {fresh}"
        )[0]

    start_all()
    machine.wait_for_unit("burrow-builds-init.service")
    machine.wait_for_unit("burrow-builds.socket")
    machine.wait_for_unit("nginx.service")

    with subtest("the cache is served, its key and nginx's fetches aren't"):
        machine.succeed("curl -fsS http://localhost/builds/nix-cache-info | grep -x 'StoreDir: /nix/store'")
        machine.succeed("curl -fsS http://localhost/builds/public-key | grep '^machine-builds-1:'")
        machine.succeed("test \"$(stat -c %a /var/lib/burrow/builds-key/secret-key)\" = 600")
        for path in ["fetched/", "tmp/lock"]:
            got = machine.succeed(f"curl -sS -o /dev/null -w '%{{http_code}}' http://localhost/builds/{path}")
            assert got == "404", f"{path}: {got}"

    with subtest("a run the door refuses writes nothing"):
        got = upload("forged", ["${one}"])
        assert got == "403", got + machine.succeed("cat /tmp/answer")
        machine.succeed("grep 'its token expired' /tmp/answer")
        assert substitute("${one}", "/tmp/b") != 0

    with subtest("a NAR that isn't its manifest's is never written"):
        machine.succeed("nix-store --dump ${two} >/tmp/two.nar")
        got = upload("public", ["${one}"], nars="/tmp/two.nar")
        assert got in ["400", "422"], got + machine.succeed("cat /tmp/answer")
        machine.fail("test -e /var/lib/burrow-builds/$(basename ${one} | cut -c1-32).narinfo")

    with subtest("an admitted upload is substituted by a store that trusts only the machine's key"):
        got = upload("public", ["${one}", "${two}", "${three}"])
        assert got == "200", got + machine.succeed("cat /tmp/answer")
        assert substitute("${three}", "/tmp/c") == 0
        machine.succeed("test -s /tmp/c${three} && test -s /tmp/c${one} && test -s /tmp/c${two}")
        # nginx said when it served it, which is what the cap reads.
        machine.succeed("test -s /var/lib/burrow-builds/fetched/$(basename ${one} | cut -c1-32).nar")
        # Another key isn't trusted.
        assert machine.execute(
            "nix copy --from http://localhost/builds --to 'local?root=/tmp/d' ${two} "
            "--option trusted-public-keys 'other-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=' --option substituters ''' "
            + fresh
        )[0] != 0

    with subtest("uploaded again, it's stored once"):
        got = upload("public", ["${one}"])
        assert got == "200", got
        answer = json.loads(machine.succeed("cat /tmp/answer"))
        assert answer["skipped"] == ["${one}"], answer

    # Last: it takes away what the subtests above write to.
    with subtest("with fetched/ gone, nginx starts and its other sites serve"):
        machine.succeed("rm -rf /var/lib/burrow-builds/fetched")
        machine.succeed("systemctl restart nginx.service")
        machine.wait_for_unit("nginx.service")
        machine.wait_for_open_port(80)
        got = machine.succeed("curl -sS -w ' %{http_code}' -H 'Host: other.test' http://localhost/")
        assert got == "other site 200", got
        # The store still serves; only its fetch log is lost.
        machine.succeed("curl -fsS http://localhost/builds/nix-cache-info | grep -x 'StoreDir: /nix/store'")
        # A NAR too, whose fetch log can't open now.
        machine.succeed("curl -fsS -o /dev/null http://localhost/builds/nar/$(basename ${one} | cut -c1-32).nar")
        machine.succeed("systemctl is-active nginx.service")
  '';
}
