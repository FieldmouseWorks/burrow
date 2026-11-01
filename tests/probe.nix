# burrow-probe for real, on a machine with a stand-in for gh: a unit's log
# holds a token and a password, which the journal keeps as they are and
# `burrow-probe ask` shows as they are (the control: the scrubber is all
# that stands between them and GitHub). An author's probe issue gets one
# comment answering every question, the journal's line there with both
# secrets gone and the rest of it kept, then is closed; a unit off the
# allowlist is never read; an issue anyone else opened runs nothing and is
# closed too; a repo GitHub doesn't call private gets no comment at all; and
# a kick to burrow-events starts a pass, as a repo's issue event does. The
# secrets are put together here, so no tracked file holds one (burrow's own
# secrets check reads them).
#
#   nix build .#checks.x86_64-linux.probe -L
{ self, pkgs }:
let
  hex = seed: builtins.hashString "sha256" "probe ${seed}";
  token = "gh" + "p_" + builtins.substring 0 36 (hex "token");
  password = "pw-" + builtins.substring 0 24 (hex "password");

  # gh api -i, as far as the probe asks it: the repo, its issues, a comment
  # and a close, from and into /var/lib/gh (the unit's /tmp is its own).
  gh = pkgs.writeShellScriptBin "gh" ''
    [ "$1" = api ] || exit 2
    shift
    method=GET path=
    while [ $# -gt 0 ]; do
      case $1 in
        -i) ;;
        -X) method=$2; shift ;;
        -H | --input) shift ;;
        *) path=$1 ;;
      esac
      shift
    done
    d=/var/lib/gh
    echo "$method $path" >>$d/asked
    reply() { printf 'HTTP/2.0 %s\r\nContent-Type: application/json\r\n\r\n%s' "$1" "$2"; }
    case "$method $path" in
      "GET repos/sam/homelab") reply 200 "$(cat $d/repo)" ;;
      "GET repos/sam/homelab/issues?"*) reply 200 "$(cat $d/issues)" ;;
      "POST repos/sam/homelab/issues/"*/comments)
        n=''${path#repos/sam/homelab/issues/}
        cat >"$d/posted-''${n%/comments}"
        reply 201 '{}' ;;
      "PATCH repos/sam/homelab/issues/"*)
        cat >"$d/closed-''${path##*/}"
        reply 200 '{}' ;;
      *) reply 404 '{"message": "Not Found"}' ;;
    esac
  '';
in
pkgs.testers.runNixOSTest {
  name = "probe";

  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.probe ];
    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      # Its user manager runs without a login, as the probe's `rc` reads it.
      linger = true;
    };
    burrow.probe = {
      enable = true;
      repo = "sam/homelab";
      authors = [ "sam" ];
      inherit gh;
      journal.units = [ "leaky.service" ];
    };
    systemd.services = {
      # A unit whose log holds what must never reach GitHub.
      leaky = {
        serviceConfig.Type = "oneshot";
        script = ''
          echo "leaky: cloning with ${token} now"
          echo "leaky: db password=${password}"
          echo "leaky: done"
        '';
      };
      # One off the allowlist.
      unlisted = {
        serviceConfig.Type = "oneshot";
        script = "echo unlisted: the probe never reads this";
      };
    };
    # A Remote Control server, as far as `rc` can tell.
    systemd.user.services."claude-rc@".serviceConfig.ExecStart = "${pkgs.coreutils}/bin/sleep infinity";
    # What GitHub says until the test says otherwise: burrow-events' own
    # start runs a pass, which finds nothing to do.
    systemd.tmpfiles.rules = [
      "d /var/lib/gh 0777 root root -"
      "f /var/lib/gh/repo 0666 root root - {\"private\": true}"
      "f /var/lib/gh/issues 0666 root root - []"
    ];
    environment.systemPackages = [ pkgs.curl ];
  };

  testScript = ''
    import json

    token = "${token}"
    password = "${password}"

    def github(name, value):
        machine.succeed(f"cat >/var/lib/gh/{name} <<'EOF'\n{json.dumps(value)}\nEOF")

    def issue(n, login, body):
        return {"number": n, "user": {"login": login}, "title": "probe", "body": body}

    def posted(n):
        return json.loads(machine.succeed(f"cat /var/lib/gh/posted-{n}"))["body"]

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("user@1001.service")
    machine.succeed("systemctl start leaky.service unlisted.service")
    machine.succeed("systemctl --user -M dev@ start claude-rc@web.service")

    with subtest("the control: the journal, and the probe here, hold the secrets as they are"):
        journal = machine.succeed("journalctl -u leaky.service")
        assert token in journal and password in journal, journal
        out = machine.succeed("burrow-probe ask journal leaky -1h now")
        assert token in out and password in out, out

    with subtest("an author's probe is answered once, scrubbed, and closed"):
        github("repo", {"private": True})
        github("issues", [
            issue(1, "sam", "journal leaky -1h now\njournal unlisted -1h now\nmemory\nunits\nrc\ndeployed\noom\nqueue\nrm -rf /"),
            issue(2, "mallory", "journal leaky -1h now"),
        ])
        machine.succeed("systemctl start burrow-probe.service")
        body = posted(1)
        assert token not in body and password not in body, body
        assert "leaky: cloning with [redacted] now" in body, body
        assert "leaky: db password=[redacted]" in body, body
        assert "leaky: done" in body, body
        assert "unlisted.service isn't on the journal allowlist" in body, body
        assert "the probe never reads this" not in body, body
        assert "RAM " in body and "The 20 cgroups holding the most" in body, body
        assert "dev's user manager, failed units:" in body, body
        assert "1 Remote Control server " in body and " web " in body and "active/running" in body, body
        assert "Running: /nix/store/" in body, body
        assert "the kernel's OOM kills" in body, body
        assert "proj isn't on here" in body, body
        assert "\"rm\" isn't a question" in body, body
        assert json.loads(machine.succeed("cat /var/lib/gh/closed-1")) == {"state": "closed", "state_reason": "completed"}

    with subtest("anyone else's runs nothing, and is closed"):
        assert "nothing was run" in posted(2), posted(2)
        assert json.loads(machine.succeed("cat /var/lib/gh/closed-2"))["state_reason"] == "not_planned"

    with subtest("a repo that isn't private gets no answer"):
        github("repo", {"private": False})
        github("issues", [issue(3, "sam", "memory")])
        machine.fail("systemctl start burrow-probe.service")
        machine.fail("test -e /var/lib/gh/posted-3")
        machine.succeed("journalctl -u burrow-probe.service | grep \"isn't private\"")

    with subtest("burrow-events starts a pass"):
        github("repo", {"private": True})
        github("issues", [issue(4, "sam", "deployed")])
        machine.wait_for_open_port(8788)
        machine.succeed("curl -fsS -d ''' http://127.0.0.1:8788/kick/probe")
        machine.wait_until_succeeds("test -e /var/lib/gh/closed-4", timeout=60)
        assert "Running: /nix/store/" in posted(4), posted(4)
  '';
}
