# The judge producer's looks (modules/autoland/judge.nix), for real: each
# judgment's engine runs in a unit of its own (autoland-judge-look@ID), as
# a user systemd makes for it, which sees only its look's folder as the
# producer lays it out (crates/burrow-judge, look.rs `lay`), reads its
# checkout but can't change it, has its login as its own credential and no
# network of its own, and leaves its answer in the one file of the folder
# it may write.
#
# And the issue's own test (burrow#1186): burrow.user, the user every
# authoring session runs as, tries to answer in the judge's place, by
# writing a look's answer or its spec, replacing its checkout, starting a
# look of its own, reading a look's login, reaching its process or asking
# the producer, and every way is refused.
#
# And a build of a head no CI reported on (autoland-judge-build@ID), as
# burrow-proof's worker runs it: a user of its own, which writes its own
# copy of the head and nothing of the producer's (not the mirror, not a
# look's checkout), reaches no network and no Nix daemon, and whose exit
# is how it came out. The crate's tests run the same
# look with fakes for systemd and GitHub, through the gate, to the
# producer's publishing; this is systemd's, polkit's and the users' half.
#
#   nix build .#checks.x86_64-linux.judge-look -L
{ self, pkgs }:
let
  state = "/var/lib/burrow/autoland-judge";
  # Claude Code, as a look runs it: it says who it ran as and what it could
  # reach, then answers with the model it was asked for.
  claude = pkgs.writeShellScriptBin "claude" ''
    model=
    prev=
    for a in "$@"; do
      [ "$prev" = --model ] && model=$a
      [ "$prev" = --add-dir ] && dir=$a
      prev=$a
    done
    cat >/dev/null
    {
      echo "uid $(id -u)"
      echo "home $HOME"
      echo "token $CLAUDE_CODE_OAUTH_TOKEN"
      echo "proxy $HTTPS_PROXY"
      test -r "$dir/head/f" && echo "reads its checkout"
      touch "$dir/head/f" 2>/dev/null && echo "WROTE its checkout"
      touch "$dir/forged" 2>/dev/null && echo "WROTE its folder"
      ls ${state} 2>/dev/null | grep -v '^looks$' | sed 's/^/SEES /'
      ls ${state}/looks 2>/dev/null | grep -vx "$(basename "$dir")" | sed 's/^/SEES look /'
      test -e /etc/claude-code/managed-settings.json && echo "SEES managed settings"
      cat /run/credentials/autoland-judge.service/judge-key >/dev/null 2>&1 && echo "READ the producer's key"
      (exec 3<>/dev/tcp/192.168.1.1/8080) 2>/dev/null && echo "REACHED the network"
      (exec 3<>/dev/tcp/127.0.0.1/8080) 2>/dev/null && echo "REACHED the host's loopback"
    } >&2
    printf '{"type":"result","subtype":"success","is_error":false,"session_id":"look-session","structured_output":{"verdict":"pass","why":"Read it."},"modelUsage":{"%s":{}}}\n' "$model"
  '';
  cn = pkgs.writeShellScriptBin "cn" "exit 0";
  # Antigravity, as a look runs it: verifies --settings (with always-proceed),
  # no --mode plan, no --sandbox, bash on PATH, isolated OAuth token file under
  # $HOME (with the source credential env stripped), read-only checkout, and no
  # network or Nix daemon access. Because burrow-engine-antigravity captures
  # child stderr for scrubbing, observations are returned in structured_output's
  # `why` (without quoting the token, which holds_login refuses).
  antigravity = pkgs.writeShellScriptBin "antigravity" ''
    model=
    dir=
    settings=
    prev=
    notes=""
    for a in "$@"; do
      [ "$prev" = --model ] && model=$a
      [ "$prev" = --add-dir ] && dir=$a
      [ "$prev" = --settings ] && settings=$a
      [ "$a" = --sandbox ] && notes="$notes PASSED_SANDBOX"
      [ "$prev" = --mode ] && notes="$notes PASSED_MODE=$a"
      prev=$a
    done
    cat >/dev/null
    notes="$notes uid=$(id -u)"
    token_file="$HOME/.gemini/antigravity-cli/antigravity-oauth-token"
    if [ -r "$token_file" ] && grep -qxF '{"access_token":"ag-the-looks-token"}' "$token_file"; then
      notes="$notes token_ok"
    fi
    [ -z "''${BURROW_ANTIGRAVITY_CREDENTIAL:-}" ] && notes="$notes env_stripped"
    case "$HTTPS_PROXY" in
      http://127.0.0.1:*) notes="$notes proxy_ok" ;;
    esac
    if [ -n "$settings" ] && grep -qF '"toolPermission":"always-proceed"' "$settings"; then
      notes="$notes settings_ok"
    fi
    command -v bash >/dev/null && notes="$notes has_bash"
    test -r "$dir/head/f" && notes="$notes reads_checkout"
    touch "$dir/head/f" 2>/dev/null && notes="$notes WROTE_checkout"
    touch "$dir/forged" 2>/dev/null && notes="$notes WROTE_folder"
    test -S /nix/var/nix/daemon-socket/socket && notes="$notes REACHED_nix"
    (exec 3<>/dev/tcp/192.168.1.1/8080) 2>/dev/null && notes="$notes REACHED_net"
    (exec 3<>/dev/tcp/127.0.0.1/8080) 2>/dev/null && notes="$notes REACHED_loopback"
    printf '{"event":"init","conversation_id":"ag-look-session","init":{"model":"%s"}}\n' "$model"
    printf '{"event":"result","result":{"conversation_id":"ag-look-session","status":"SUCCESS","response":"{\\"verdict\\":\\"pass\\",\\"why\\":\\"%s\\"}","structured_output":{"verdict":"pass","why":"%s"}}}\n' "$notes" "$notes"
  '';
  git = "${pkgs.git}/bin/git";
in
pkgs.testers.runNixOSTest {
  name = "judge-look";

  nodes.machine = { lib, ... }: {
    imports = [
      self.nixosModules.autoland
      self.nixosModules.agents
      self.nixosModules.antigravity
      self.nixosModules.ledger
      self.nixosModules.github
    ];
    burrow.engines.antigravity = {
      enable = true;
      package = antigravity;
      credentialFile = "/run/keys/ag-token";
      credentialMode = "home-file";
      hosts = [ "*.googleapis.com" ];
    };
    burrow.ledger = {
      enable = true;
      package = cn;
      owners = [ "acme" ];
      anchorRepo = "acme/ledger";
      reviewers = [ "steward[bot]" ];
      controllerUser = "conary-flow";
    };
    burrow.events.enable = lib.mkForce false;
    # Its App, which nothing here asks: the crate's tests are GitHub's half.
    burrow.github = {
      apps.workers = {
        id = 7;
        accountId = 8;
        slug = "acme-workers";
        keyFile = "/run/keys/workers.pem";
        api = "http://127.0.0.1:8080";
      };
      machinery = "workers";
    };
    burrow.claude.package = claude;
    burrow.autoland.plugins.judge = {
      enable = true;
      producer = "acme/judge";
      keyFile = "/run/keys/judge";
      sealFile = "/run/keys/seal";
      claudeTokenFile = "/run/keys/claude-token";
    };
    # Started by the test, once its keys are there.
    systemd.services.autoland-judge.wantedBy = lib.mkForce [ ];
    users.users.dev.isNormalUser = true;
  };

  testScript = ''
    import json
    import shlex

    def producer(cmd, ok=True):
        run = f"runuser -u autoland-judge -- bash -c {shlex.quote('umask 0027; ' + cmd)}"
        return machine.succeed(run) if ok else machine.fail(run)

    def unit(id):
        return f"autoland-judge-look-claude@{id}.service"

    def folder(id):
        return f"${state}/looks/{id}"

    def shown(id):
        out = machine.succeed(f"systemctl show -p ActiveState,SubState,Result,ExecMainCode,ExecMainStatus {unit(id)}")
        return dict(l.split("=", 1) for l in out.splitlines())

    # A look's folder as the producer lays it out (look.rs, `lay`).
    def lay(id):
        spec = {"engine": "claude", "repo": "acme/widget", "prompt": "Judge it.", "model": "a-model", "effort": "high"}
        producer(" && ".join([
            f"mkdir -m 0750 {folder(id)} {folder(id)}/head {folder(id)}/base",
            f"printf %s {shlex.quote(json.dumps(spec))} >{folder(id)}/spec.json",
            f"echo code >{folder(id)}/head/f",
            f"echo diff >{folder(id)}/changes.diff",
            f": >{folder(id)}/answer.json && chmod 0660 {folder(id)}/answer.json",
        ]))

    def said(id):
        return machine.succeed(f"journalctl -u {shlex.quote(unit(id))} -o cat")

    def build_unit(id):
        return f"autoland-judge-build@{id}.service"

    # A build's folder as the producer lays it out (build.rs, burrow-proof's
    # `lay`): its spec, its log for it to write, and its own copy of the
    # head, a mirror of a repo.
    def lay_build(id, script):
        run = f"${state}/runs/{id}"
        producer(" && ".join([
            "src=$(mktemp -d) && cd $src",
            "${git} init -q . && echo code >f && ${git} add f",
            "${git} -c user.name=t -c user.email=t@t commit -qm head",
            f"mkdir -m 0750 -p {run} && ${git} clone -q --mirror . {run}/repo.git",
            f": >{run}/log && chmod 0660 {run}/log",
            f"${git} rev-parse HEAD HEAD^{{tree}} >{run}/.ids",
        ]))
        head, tree = producer(f"cat {run}/.ids").split()
        spec = {"mode": "proof", "repo": "acme/widget", "head": head, "tree": tree,
                "check": "gate", "recipe": {"Run": ["bash", "-c", script]}}
        producer(f"printf %s {shlex.quote(json.dumps(spec))} >{run}/spec.json")

    def build_shown(id):
        out = machine.succeed(f"systemctl show -p ActiveState,SubState,Result,ExecMainCode,ExecMainStatus {build_unit(id)}")
        return dict(l.split("=", 1) for l in out.splitlines())

    machine.wait_for_unit("multi-user.target")
    # Something to reach on the host, were a look able to.
    machine.succeed("systemd-run --unit=listener ${pkgs.python3}/bin/python3 -m http.server 8080")
    machine.wait_for_open_port(8080)
    # The machine's own address, as the stand-in tries it: reachable from
    # outside a look.
    machine.succeed("bash -c 'exec 3<>/dev/tcp/192.168.1.1/8080'")

    with subtest("the producer starts with its keys, as a user of its own, for the controller alone"):
        machine.succeed("install -d -m 0700 /run/keys")
        machine.succeed("head -c 32 /dev/urandom >/run/keys/judge")
        machine.succeed("printf %s abababababababababababababababababababababababababababababababab >/run/keys/seal")
        machine.succeed("printf 'sk-the-looks-token\\n' >/run/keys/claude-token")
        machine.succeed("printf '{\"access_token\":\"ag-the-looks-token\"}\\n' >/run/keys/ag-token")
        machine.succeed("chmod 0400 /run/keys/*")
        machine.succeed("systemctl start autoland-judge.service")
        machine.wait_for_file("/run/conary-producers/judge/producer.sock")
        assert machine.succeed("stat -c '%U %G %a' ${state} ${state}/looks").split("\n")[:2] == ["autoland-judge autoland-judge 750"] * 2
        machine.fail("runuser -u dev -- test -e /run/conary-producers/judge/producer.sock")

    with subtest("a look is a user of its own, reads its checkout, has its login and nothing else, and answers"):
        lay("a1")
        machine.succeed(f"runuser -u autoland-judge -- systemctl start {unit('a1')}")
        s = shown("a1")
        assert (s["ActiveState"], s["Result"], s["ExecMainStatus"]) == ("active", "success", "0"), (s, said("a1"))
        out = said("a1")
        uid = int(out.split("uid ")[1].split()[0])
        assert uid not in (0, int(machine.succeed("id -u autoland-judge")), int(machine.succeed("id -u dev"))), out
        assert "reads its checkout" in out, out
        assert "token sk-the-looks-token" in out, out
        assert "proxy http://127.0.0.1:" in out, out
        for never in ["WROTE", "SEES", "READ the producer", "REACHED"]:
            assert never not in out, out
        answer = json.loads(producer(f"cat {folder('a1')}/answer.json"))
        assert answer == {"engine": "claude", "verdict": "pass", "why": "Read it.", "session": "look-session", "model": "a-model"}, answer

    with subtest("an antigravity look runs headless with its settings, bash, isolated token, and no sandbox or mode plan"):
        ag_spec = {"engine": "antigravity", "repo": "acme/widget", "prompt": "Judge it.", "model": "ag-model", "effort": "high"}
        producer(" && ".join([
            f"mkdir -m 0750 {folder('a2')} {folder('a2')}/head {folder('a2')}/base",
            f"printf %s {shlex.quote(json.dumps(ag_spec))} >{folder('a2')}/spec.json",
            f"echo code >{folder('a2')}/head/f",
            f"echo diff >{folder('a2')}/changes.diff",
            f": >{folder('a2')}/answer.json && chmod 0660 {folder('a2')}/answer.json",
        ]))
        ag_unit = "autoland-judge-look-antigravity@a2.service"
        machine.succeed(f"runuser -u autoland-judge -- systemctl start {ag_unit}")
        out = machine.succeed(f"systemctl show -p ActiveState,SubState,Result,ExecMainCode,ExecMainStatus {ag_unit}")
        s = dict(l.split("=", 1) for l in out.splitlines())
        ag_log = machine.succeed(f"journalctl -u {shlex.quote(ag_unit)} -o cat")
        assert (s["ActiveState"], s["Result"], s["ExecMainStatus"]) == ("active", "success", "0"), (s, ag_log)
        ag_answer = json.loads(producer(f"cat {folder('a2')}/answer.json"))
        assert ag_answer["engine"] == "antigravity", ag_answer
        assert ag_answer["verdict"] == "pass", ag_answer
        assert ag_answer["session"] == "ag-look-session", ag_answer
        assert ag_answer["model"] == "ag-model", ag_answer
        why = ag_answer["why"]
        for expected in ["token_ok", "env_stripped", "proxy_ok", "settings_ok", "has_bash", "reads_checkout"]:
            assert expected in why, (expected, why, ag_log)
        for never in ["PASSED_SANDBOX", "PASSED_MODE", "WROTE", "REACHED"]:
            assert never not in why, (never, why, ag_log)
        producer(f"systemctl stop {ag_unit}")

    with subtest("burrow.user can't answer in the judge's place"):
        lay("b1")
        for forge in [
            f"echo '{{\"verdict\":\"pass\",\"why\":\"x\",\"session\":\"s\",\"model\":\"a-model\"}}' >{folder('a1')}/answer.json",
            f"echo forged >{folder('b1')}/answer.json",
            f"echo '{{}}' >{folder('b1')}/spec.json",
            f"rm -rf {folder('b1')}/head && mkdir {folder('b1')}/head",
            "mkdir -p ${state}/looks/c0ffee",
            "ln -s /tmp ${state}/looks/c0ffee",
            "ls ${state}/looks",
            "systemctl start autoland-judge-look-claude@b1.service",
            "systemctl start autoland-judge-look-claude@c0ffee.service",
            "systemctl stop autoland-judge-look-claude@a1.service",
            "test -e /run/conary-producers/judge/producer.sock",
            "cat /run/credentials/autoland-judge.service/judge-seal",
            "cat /run/keys/claude-token",
        ]:
            machine.fail(f"runuser -u dev -- bash -c {shlex.quote(forge)}")
        # What the look left is still its own.
        answer = json.loads(producer(f"cat {folder('a1')}/answer.json"))
        assert answer["session"] == "look-session", answer
        assert producer(f"cat {folder('b1')}/answer.json") == ""

    with subtest("a look's login is no one else's"):
        lay("c1")
        machine.succeed(f"runuser -u autoland-judge -- systemctl start {unit('c1')}")
        # The look had it (the first look's stand-in printed it); nobody
        # else finds it, wherever systemd keeps credentials.
        for who in ["dev", "autoland-judge", "conary-flow"]:
            machine.fail(f"runuser -u {who} -- grep -rqs sk-the-looks-token /run/credentials /run/keys")

    with subtest("only the producer starts and stops looks, and nothing else"):
        machine.fail("runuser -u conary-flow -- systemctl start autoland-judge-look-claude@d1.service")
        producer("systemctl start autoland-judge-look-claude@not-a-look.service", ok=False)
        producer("systemctl restart autoland-judge.service", ok=False)
        producer("systemctl stop nix-daemon.service", ok=False)
        producer(f"systemctl stop {unit('a1')} {unit('b1')} {unit('c1')}")

    with subtest("a build is a user of its own, writes its own copy alone, reaches nothing, and its exit is how it came out"):
        lay("f1")
        lay_build("b0", "\n".join([
            "echo uid $(id -u)",
            "echo built >f && grep -q built f && echo wrote its own copy",
            "touch ${state}/looks/f1/head/f 2>/dev/null && echo WROTE a look checkout",
            "touch ${state}/forged 2>/dev/null && echo WROTE the producer state",
            "ls ${state}/looks ${state}/git 2>/dev/null | sed 's/^/SEES /'",
            "test -e /run/credentials/autoland-judge.service/judge-key && echo SEES the producer key",
            "test -S /nix/var/nix/daemon-socket/socket && echo REACHED the Nix daemon",
            "(exec 3<>/dev/tcp/192.168.1.1/8080) 2>/dev/null && echo REACHED the network",
            "(exec 3<>/dev/tcp/127.0.0.1/8080) 2>/dev/null && echo REACHED the host loopback",
            "true",
        ]))
        machine.succeed(f"runuser -u autoland-judge -- systemctl start {build_unit('b0')}")
        s = build_shown("b0")
        log = producer("cat ${state}/runs/b0/log")
        assert (s["ActiveState"], s["Result"], s["ExecMainStatus"]) == ("active", "success", "0"), (s, log)
        # What it printed, not the worker's note of its command.
        printed = [l for l in log.splitlines() if not l.startswith("burrow-proof:")]
        uid = int(next(l for l in printed if l.startswith("uid "))[4:])
        assert uid not in (0, int(machine.succeed("id -u autoland-judge")), int(machine.succeed("id -u dev"))), log
        assert "wrote its own copy" in printed, log
        # Every way past its folder, at once, so a run with a guard removed
        # names all it let through.
        reached = [l for l in printed if l.startswith(("WROTE", "SEES", "REACHED"))]
        assert reached == [], (reached, log)
        assert producer("cat ${state}/looks/f1/head/f") == "code\n"

        lay_build("b1", "echo 'test widget ... FAILED'; exit 1")
        machine.succeed(f"runuser -u autoland-judge -- systemctl start {build_unit('b1')}")
        s = build_shown("b1")
        assert (s["ActiveState"], s["Result"], s["ExecMainStatus"]) == ("active", "success", "1"), s
        assert "test widget ... FAILED" in producer("cat ${state}/runs/b1/log")

        # A program it needs isn't in the unit: its worker's word that it
        # couldn't run it, kept as the unit's success, an outcome the look
        # is shown.
        lay_build("b2", "python3 -c 'print(1)'")
        machine.succeed(f"runuser -u autoland-judge -- systemctl start {build_unit('b2')}")
        s = build_shown("b2")
        assert (s["ActiveState"], s["Result"], s["ExecMainStatus"]) == ("active", "success", "2"), s

        # Only the producer starts and stops them.
        machine.fail(f"runuser -u dev -- systemctl start {build_unit('b9')}")
        machine.fail(f"runuser -u dev -- systemctl stop {build_unit('b0')}")
        producer(f"systemctl stop {build_unit('b0')} {build_unit('b1')} {build_unit('b2')}")

    with subtest("its looks end with it, and a restart clears their folders"):
        lay("e1")
        machine.succeed(f"runuser -u autoland-judge -- systemctl start {unit('e1')}")
        machine.succeed("systemctl restart autoland-judge.service")
        machine.wait_for_file("/run/conary-producers/judge/producer.sock")
        machine.wait_until_succeeds(f"test ! -e {folder('e1')}", timeout=30)
        machine.wait_until_fails(f"systemctl list-units --all --plain --no-legend {unit('e1')} | grep -q .", timeout=30)
  '';
}
