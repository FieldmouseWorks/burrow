# burrow-probe for real, on a machine with a stand-in for gh: a unit's log
# holds a token and a password, which the journal keeps as they are and
# `burrow-probe ask` shows as they are (the control: the scrubber is all
# that stands between them and GitHub). An author's probe issue gets one
# comment answering every question, the journal's line there with both
# secrets gone and the rest of it kept, then is closed; a unit off the
# allowlist is never read; an issue anyone else opened runs nothing and is
# closed too; a repo GitHub doesn't call private gets no comment at all; and
# a kick to burrow-events starts a pass, as a repo's issue event does.
# `runners` tells a named runner and the fresh runners' template apart from
# what's found on their PATH, with no rustc, no rustup and no default
# toolchain, never reading a path the reviewers never read; `trust` says the
# deployed authors; `issue` gives an issue's writers and retained edits. The
# secrets are put together here, so no tracked file holds one (burrow's own
# secrets check reads them).
#
#   nix build .#checks.x86_64-linux.probe -L
{ self, pkgs }:
let
  hex = seed: builtins.hashString "sha256" "probe ${seed}";
  token = "gh" + "p_" + builtins.substring 0 36 (hex "token");
  password = "pw-" + builtins.substring 0 24 (hex "password");

  # gh api -i, as far as the probe asks it: the repo, its issues, who wrote
  # one (its opener, and the body's editors from edits-N, none by default),
  # a comment and a close, from and into /var/lib/gh (the unit's /tmp is its
  # own).
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
      "POST graphql")
        n=$(${pkgs.jq}/bin/jq -r .variables.n)
        edits=$(cat "$d/edits-$n" 2>/dev/null || echo '[]')
        reply 200 "$(${pkgs.jq}/bin/jq -c --argjson n "$n" --argjson edits "$edits" '
          .[] | select(.number == $n)
          | { data: { repository: { issue: {
              author: { login: .user.login, databaseId: .user.id }, title, body,
              userContentEdits: { totalCount: ($edits | length), nodes: $edits },
              timelineItems: { totalCount: 0, nodes: [] } } } } }' $d/issues)" ;;
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

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.probe self.nixosModules.dispatch ];
    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      # Its user manager runs without a login, as the probe's `rc` reads it.
      linger = true;
    };
    burrow.probe = {
      enable = true;
      repo = "sam/homelab";
      authors = [ { login = "sam"; id = 1; } ];
      inherit gh;
      journal.units = [ "leaky.service" ];
      hidden = [ "(^|/)secrets?/" ];
    };
    # A named runner, whose PATH has no Rust, a directory linked into a
    # hidden one, and a rustc that leads to a hidden file; and the fresh
    # runners' template.
    burrow.dispatch = {
      enable = true;
      runners.ci-runner = { repo = "sam/homelab"; labels = [ "homelab" ]; };
      fresh = { enable = true; owner = "sam"; repos = [ "sam/homelab" ]; };
    };
    users.users.ci = { isSystemUser = true; group = "ci"; home = "/var/lib/ci"; createHome = true; homeMode = "755"; };
    users.groups.ci = { };
    systemd.services.ci-runner = {
      path = [ pkgs.jq ];
      environment.PATH = lib.mkForce "/opt/runner/bin:/opt/sneaky:${pkgs.coreutils}/bin";
      serviceConfig.User = "ci";
      script = "exec sleep infinity";
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
      "d /run/secrets 0755 root root -"
      "f /run/secrets/rustc 0755 root root - SECRET-RUSTC"
      "d /run/secrets/bin 0755 root root -"
      "d /opt/runner/bin 0755 root root -"
      "L+ /opt/runner/bin/git - - - - ${pkgs.git}/bin/git"
      "L+ /opt/runner/bin/rustc - - - - /run/secrets/rustc"
      "L+ /opt/sneaky - - - - /run/secrets/bin"
    ];
    environment.systemPackages = [ pkgs.curl pkgs.util-linux ];
  };

  testScript = ''
    import json

    token = "${token}"
    password = "${password}"

    def github(name, value):
        machine.succeed(f"cat >/var/lib/gh/{name} <<'EOF'\n{json.dumps(value)}\nEOF")

    def issue(n, login, body):
        return {"number": n, "user": {"login": login, "id": 1 if login == "sam" else 99}, "title": "probe", "body": body}

    def posted(n):
        return json.loads(machine.succeed(f"cat /var/lib/gh/posted-{n}"))["body"]

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("user@1001.service")
    machine.succeed("systemctl start leaky.service unlisted.service")
    machine.succeed("systemctl --user -M dev@ start claude-rc@web.service")
    # Structured coredump journal fixtures: exercise the real journalctl
    # selection without crashing a process or needing any core-file access.
    machine.succeed("logger --journald <<'EOF'\n"
        "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1\n"
        "COREDUMP_EXE=/bin/probe-example\nCOREDUMP_PID=123\nCOREDUMP_SIGNAL=6\n"
        "COREDUMP_USER_UNIT=proj-example.service\n"
        f"MESSAGE=Stack trace of thread 123: abort token={token}\n"
        "COREDUMP_ENVIRON=plain private environment\n"
        "COREDUMP_CMDLINE=plain private argument\nEOF")
    machine.succeed("logger --journald <<'EOF'\n"
        "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1\n"
        "COREDUMP_EXE=/bin/other-example\nMESSAGE=unrelated crash\nEOF\njournalctl --sync")

    with subtest("the control: the journal, and the probe here, hold the secrets as they are"):
        journal = machine.succeed("journalctl -u leaky.service")
        assert token in journal and password in journal, journal
        out = machine.succeed("burrow-probe ask journal leaky -1h now")
        assert token in out and password in out, out
        crash = machine.succeed("burrow-probe ask crashes /bin/probe-example -1h")
        assert "COREDUMP_PID: 123" in crash and token in crash, crash
        assert "plain private" not in crash and "unrelated crash" not in crash, crash

    with subtest("an author's probe is answered once, scrubbed, and closed"):
        github("repo", {"private": True})
        github("issues", [
            issue(1, "sam", "journal leaky -1h now\njournal unlisted -1h now\ncrashes /bin/probe-example -1h\nmemory\nunits\nrc\ndeployed\noom\nqueue\nrm -rf /"),
            issue(2, "mallory", "journal leaky -1h now"),
        ])
        machine.succeed("systemctl start burrow-probe.service")
        body = posted(1)
        assert token not in body and password not in body, body
        assert "leaky: cloning with [redacted] now" in body, body
        assert "leaky: db password=[redacted]" in body, body
        assert "leaky: done" in body, body
        assert "COREDUMP_PID: 123" in body and "proj-example.service" in body, body
        assert "Stack trace of thread 123: abort token=[redacted]" in body, body
        assert "plain private" not in body and "unrelated crash" not in body, body
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

    with subtest("runners: configured, loaded and found, with no Rust and nothing hidden read"):
        machine.succeed("systemctl start ci-runner.service")
        out = machine.succeed("burrow-probe ask runners")
        assert "ci-runner: a named runner for sam/homelab (labels: self-hosted, Linux, X64, homelab)" in out, out
        assert "configured: user ci, HOME /var/lib/ci, PATH /opt/runner/bin:/opt/sneaky:" in out, out
        assert "loaded: ci-runner.service active/running" in out, out
        assert "/opt/sneaky (not read: it is, or leads to, a path the reviewers never read)" in out, out
        assert "rustc in /opt/runner/bin, not read" in out, out
        assert "cargo not on PATH" in out and "rustup not on PATH" in out, out
        assert "default toolchain: unknown: a toolchain command on PATH isn't read" in out, out
        assert "burrow-fresh-runner@INSTANCE: runners started fresh for one job each, for sam/homelab" in out, out
        assert "configured: user a dynamic user of its own, HOME /var/lib/private/burrow-fresh-runner/%i" in out, out
        assert "loaded: no instance is loaded now" in out, out
        assert "default toolchain: none: neither rustc nor rustup is on PATH" in out, out
        assert "SECRET-RUSTC" not in out, out
        full = machine.succeed("burrow-probe ask runners ci-runner")
        assert "/opt/runner/bin: 1 commands from git-" in full and "1 left out as hidden" in full, full
        assert "found tools: git in /opt/runner/bin (git-" in full, full
        machine.fail("burrow-probe ask runners nobody")

    with subtest("trust and issue: deployed authors, and an issue's writers without its text"):
        github("repo", {"private": True})
        github("issues", [issue(6, "sam", "trust\nissue sam/homelab 6\nissue sam/homelab 0")])
        github("edits-6", [{"editor": {"login": "sam", "databaseId": 1}, "deletedBy": None}] * 2)
        machine.succeed("systemctl start burrow-probe.service")
        body = posted(6)
        assert "- probe (issues in sam/homelab): sam (1)" in body, body
        assert "- a Claude job with no --app: dev's own gh login" in body, body
        assert "sam/homelab#6: " in body, body
        assert "Body edits: 2 edits, all retained and returned" in body, body
        assert "- sam (an account 1): opened it, 2 edits; a probe author" in body, body
        assert "`issue` takes OWNER/REPO and an issue number" in body, body

    with subtest("anyone else's runs nothing, and is closed"):
        assert "nothing was run" in posted(2), posted(2)
        assert json.loads(machine.succeed("cat /var/lib/gh/closed-2"))["state_reason"] == "not_planned"

    with subtest("an author's probe someone else edited runs nothing, and is closed"):
        github("issues", [issue(5, "sam", "journal leaky -1h now")])
        github("edits-5", [{"editor": {"login": "sam", "databaseId": 1}, "deletedBy": None}, {"editor": {"login": "mallory", "databaseId": 99}, "deletedBy": None}])
        machine.succeed("systemctl start burrow-probe.service")
        assert "written or changed by someone other than sam" in posted(5), posted(5)
        assert "leaky:" not in posted(5), posted(5)
        assert json.loads(machine.succeed("cat /var/lib/gh/closed-5"))["state_reason"] == "not_planned"

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
