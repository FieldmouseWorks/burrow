# The flow's controller (burrow.ledger.controllerUser, Conary's
# docs/m6-plugins.md, B3): a user of its own that the flow, its workers and
# so its plugins run as, and the only user that may open a producer's
# socket. Producers listen each in a directory of their own under one only
# the controller's group may enter, so burrow.user, whom every authoring
# session runs as, can't reach one at all; each producer checks its peer
# too (crates/burrow-producer). The controller signs the ledger as
# burrow.user's writers do (a flow with a train or evidence signs its ops),
# through ACLs naming it: on a ledger set up after it, and on one that was
# burrow.user's alone before. Nobody else gets the key, a producer in the
# controller's group included, a link the controller puts in the ledger
# leads that access nowhere else, and a controller changed or turned off
# keeps nothing. With the option off nothing changes.
#
# cn here is a stand-in that touches the ledger's files as Conary's writer
# does (conary-core's log.rs, conary-github's bases.rs and etags.rs): it
# proves the permissions, not a real conaryd's train.
#
#   nix build .#checks.x86_64-linux.ledger-controller -L
{ self, pkgs, accessTests }:
let
  cn = pkgs.writeShellApplication {
    name = "cn";
    runtimeInputs = [ pkgs.coreutils pkgs.util-linux ];
    text = ''
      d=''${CONARY_LEDGER:-}
      # One op, signed: under the lock, with the key read, appended to the
      # log and the merge bases in place, and the anchor renamed in.
      sign() {
        exec 9>>"$d/lock"
        flock 9
        [ "$(head -c 32 "$d/key" | wc -c)" = 32 ]
        echo "$(id -un) $1" >>"$d/ops.jsonl"
        echo "$1 base" >>"$d/merge-bases"
        echo "$(wc -l <"$d/ops.jsonl") $(id -un)" >"$d/.anchor.$$.new"
        mv -f "$d/.anchor.$$.new" "$d/anchor"
      }
      case $1 in
        # As Conary sets a ledger up: its key made readable by its owner
        # only and linked into place, the rest renamed in, and the etags
        # cache the writer's alone.
        init)
          (umask 077; head -c 32 /dev/urandom >"$d/.key.new")
          ln "$d/.key.new" "$d/key"
          rm "$d/.key.new"
          echo public >"$d/.key.pub.new"
          mv "$d/.key.pub.new" "$d/key.pub"
          : >>"$d/lock"
          : >>"$d/ops.jsonl"
          mkdir -m 0700 "$d/etags"
          # An answer, 0600 as Conary's tempfile makes it: a default ACL
          # would override a umask.
          echo etag >"$d/etags/h1"
          chmod 600 "$d/etags/h1"
          echo "0 $(id -un)" >"$d/anchor"
          ;;
        sync | sign) sign "$2" ;;
      esac
    '';
  };
  gh = pkgs.writeShellScriptBin "gh" ''
    case "$*" in
      "auth token") echo gho_stand_in ;;
      *) exit 1 ;;
    esac
  '';
  ledger = {
    enable = true;
    package = cn;
    inherit gh;
    owners = [ "sam" ];
    anchorRepo = "sam/homelab";
    reviewers = [ "steward[bot]" ];
  };
  python = "${pkgs.python3}/bin/python3";
  # A producer's socket as the kit binds it: open to anyone (0666), its
  # peer check the producer's own gate.
  listen = pkgs.writeText "listen.py" ''
    import os, socket
    path = os.path.join(os.environ["RUNTIME_DIRECTORY"], "p.sock")
    s = socket.socket(socket.AF_UNIX)
    s.bind(path)
    os.chmod(path, 0o666)
    s.listen()
    while True:
        c, _ = s.accept()
        c.sendall(b"ok\n")
        c.close()
  '';
  ask = pkgs.writeText "ask.py" ''
    import socket, sys
    s = socket.socket(socket.AF_UNIX)
    s.connect("/run/conary-producers/test/p.sock")
    sys.stdout.write(s.makefile().readline())
  '';
  node = controller: { config, lib, ... }: {
    imports = [ self.nixosModules.ledger ];
    burrow.ledger = ledger // lib.optionalAttrs controller { controllerUser = "conary-flow"; };
    burrow.events.enable = lib.mkForce false;
    users.users.dev.isNormalUser = true;
    # Someone else in burrow.user's group, who never gets the key.
    users.users.other = { isNormalUser = true; group = config.users.users.dev.group; };
    environment.systemPackages = [ pkgs.acl ];
    # A producer, as a user of its own, wired as a producer's unit is.
    systemd.services.producer = lib.mkIf (config.burrow.ledger.controllerUser != null) {
      serviceConfig = config.burrow.ledger.flow.producer "test" // {
        User = "nobody";
        ExecStart = "${python} ${listen}";
      };
    };
    # The controller changed, and turned off.
    specialisation = lib.mkIf controller {
      moved.configuration.burrow.ledger.controllerUser = lib.mkForce "conary-flow2";
      retired.configuration.burrow.ledger.controllerUser = lib.mkForce null;
    };
    # The flow signing an op (a train's join, leave or step, evidence), as
    # the flow's units run: as its user, on its state.
    systemd.services."flow@" = {
      environment = config.burrow.ledger.env // config.burrow.ledger.flow.env;
      serviceConfig = {
        Type = "oneshot";
        User = config.burrow.ledger.flow.user;
        ExecStart = "${cn}/bin/cn sign %i";
      };
    };
  };
in
pkgs.testers.runNixOSTest {
  name = "ledger-controller";

  nodes.on = node true;
  nodes.off = node false;

  testScript = ''
    ledger = "/var/lib/burrow/conary"
    env = f"env CONARY_LEDGER={ledger}"

    def ops(m):
        return [l.split(" ", 1) for l in m.succeed(f"cat {ledger}/ops.jsonl").splitlines()]

    def nobody_else_reads_the_key():
        # Not burrow.user's group, not anyone, and not a producer, which
        # is in the controller's group.
        on.fail(f"runuser -u other -- cat {ledger}/key")
        on.fail(f"runuser -u nobody -- cat {ledger}/key")
        on.fail(f"systemd-run --wait --pipe --quiet -p User=nobody -p SupplementaryGroups=conary-flow cat {ledger}/key")
        on.succeed(f"runuser -u conary-flow -- test -r {ledger}/key")
        on.fail(f"runuser -u conary-flow -- test -w {ledger}/key")
        # The etags cache stays the writer's alone: the flow reads none, and
        # neither does burrow.user's group (other is in it).
        on.fail(f"runuser -u conary-flow -- ls {ledger}/etags")
        on.fail(f"runuser -u other -- ls {ledger}/etags")
        assert on.succeed(f"stat -c '%U %a' {ledger}/etags").strip() == "dev 700", "etags isn't widened"
        # Nor does what it holds name anyone.
        assert on.succeed(f"stat -c '%U %a' {ledger}/etags/h1").strip() == "dev 600"
        assert on.succeed(f"getfacl -cps {ledger}/etags {ledger}/etags/h1") == ""
        # The directory stats as 775 (its ACL's mask): still nobody else's to write.
        on.fail(f"runuser -u other -- touch {ledger}/x")
        on.fail(f"runuser -u nobody -- touch {ledger}/x")

    start_all()
    on.wait_for_unit("multi-user.target")
    off.wait_for_unit("multi-user.target")
    # Its ACL lines name files a ledger not set up yet doesn't have.
    on.fail("systemctl is-failed systemd-tmpfiles-setup.service")

    with subtest("the controller is a user of its own, with state only it can read"):
        on.succeed("id conary-flow")
        on.fail("id -nG dev | grep -w conary-flow")
        assert on.succeed("stat -c '%U %G %a' /var/lib/burrow/flow").strip() == "conary-flow conary-flow 700"
        on.fail("runuser -u dev -- ls /var/lib/burrow/flow")
        assert on.succeed("stat -c '%U %G %a' /run/conary-producers").strip() == "root conary-flow 710"

    with subtest("burrow.user can't open a producer's socket, and a flow worker can"):
        on.succeed("systemctl start producer.service")
        on.wait_for_unit("producer.service")
        on.wait_for_file("/run/conary-producers/test/p.sock")
        err = on.fail("runuser -u dev -- ${python} ${ask} 2>&1")
        assert "Permission denied" in err, err
        assert on.succeed("runuser -u conary-flow -- ${python} ${ask}") == "ok\n"
        # As a unit of the flow's runs: as its user, nothing more.
        assert on.succeed("systemd-run --wait --pipe --quiet -p User=conary-flow ${python} ${ask}") == "ok\n"
        assert on.succeed("stat -c '%U %G %a' /run/conary-producers").strip() == "root conary-flow 710"

    with subtest("on a ledger set up after it, the flow and burrow.user both sign"):
        on.succeed("systemctl start burrow-ledger@sam-r.service")
        # The key the first sync made gets the controller's access at once.
        on.wait_until_succeeds(f"runuser -u conary-flow -- test -r {ledger}/key", timeout=60)
        on.wait_for_unit("burrow-ledger-access.service")
        # Its next run, as the next boot or switch: the etags cache the
        # first sync made under the directory's default entries.
        on.succeed("systemctl restart burrow-ledger-access.service")
        # So the flow makes the merge bases, and burrow.user appends to them.
        on.succeed(f"rm {ledger}/merge-bases")
        for op in ["train-join", "train-step", "evidence", "train-leave"]:
            on.succeed(f"systemctl start flow@{op}.service")
        on.succeed("systemctl start burrow-ledger@sam-r.service")
        on.succeed(f"runuser -u dev -- {env} cn sign by-hand")
        assert ops(on) == [
            ["dev", "sam/r"],
            ["conary-flow", "train-join"], ["conary-flow", "train-step"],
            ["conary-flow", "evidence"], ["conary-flow", "train-leave"],
            ["dev", "sam/r"], ["dev", "by-hand"],
        ], ops(on)
        assert on.succeed(f"stat -c %U {ledger}/merge-bases").strip() == "conary-flow"
        assert on.succeed(f"tail -n1 {ledger}/merge-bases") == "by-hand base\n"
        # Everyone still reads the log; the key stays as it was made.
        on.succeed(f"runuser -u nobody -- cat {ledger}/ops.jsonl {ledger}/anchor")
        assert on.succeed(f"stat -c '%U %a' {ledger}/key").strip() == "dev 640", "the key's group bits are its ACL's mask"
        nobody_else_reads_the_key()

    with subtest("on a ledger burrow.user had alone before, a switch lets the flow sign"):
        on.succeed(f"setfacl -R -b {ledger} && chown -R dev {ledger} && chmod 600 {ledger}/key && chmod 644 {ledger}/ops.jsonl {ledger}/anchor {ledger}/merge-bases {ledger}/lock && chmod 700 {ledger}/etags")
        on.fail("systemctl start flow@before.service")
        # As the first switch to it does.
        on.succeed("systemctl restart burrow-ledger-access.service")
        on.succeed("systemctl start flow@after.service")
        on.succeed("systemctl start burrow-ledger@sam-r.service")
        on.succeed("systemctl start flow@again.service")
        on.succeed(f"runuser -u dev -- {env} cn sign by-hand")
        assert [o for o in ops(on)[7:]] == [
            ["conary-flow", "after"], ["dev", "sam/r"],
            ["conary-flow", "again"], ["dev", "by-hand"],
        ], ops(on)
        nobody_else_reads_the_key()

    with subtest("a link the controller puts in the ledger leads its access nowhere"):
        # Files outside the ledger, root's and burrow.user's own, and the
        # ledger's key: the controller swaps a file it may write, and the
        # key, for links to them, and the access runs again as root.
        on.succeed("echo root >/var/lib/root-secret && chmod 600 /var/lib/root-secret")
        on.succeed("echo dev >/var/lib/dev-secret && chown dev /var/lib/dev-secret && chmod 600 /var/lib/dev-secret")
        as_flow = "runuser -u conary-flow -- sh -c"
        on.succeed(f"{as_flow} 'ln -sfn /var/lib/root-secret {ledger}/merge-bases'")
        on.succeed(f"{as_flow} 'ln -sfn {ledger}/key {ledger}/lock'")
        on.succeed(f"{as_flow} 'ln -s /var/lib/dev-secret {ledger}/.k && mv -fT {ledger}/.k {ledger}/key'")
        on.succeed("systemctl restart burrow-ledger-access.service")
        for f, owner in [("/var/lib/root-secret", "root"), ("/var/lib/dev-secret", "dev")]:
            assert on.succeed(f"stat -c '%U %a' {f}").strip() == f"{owner} 600", f
            assert on.succeed(f"getfacl -cps {f}") == "", f
            on.fail(f"runuser -u conary-flow -- cat {f}")
        # The links are burrow.user's now, and still links.
        assert on.succeed(f"stat -c '%U %F' {ledger}/merge-bases {ledger}/lock {ledger}/key") == "dev symbolic link\n" * 3
        # The ledger as it was: the key back from the last sync's, and the
        # files the writers open in place made again.
        on.succeed(f"rm {ledger}/merge-bases {ledger}/lock {ledger}/key")
        on.succeed(f"runuser -u dev -- sh -c 'head -c 32 /dev/urandom >{ledger}/key && chmod 600 {ledger}/key && touch {ledger}/lock {ledger}/merge-bases'")
        on.succeed("systemctl restart burrow-ledger-access.service")
        on.succeed("systemctl start flow@relinked.service")
        on.succeed(f"runuser -u dev -- {env} cn sign by-hand")
        assert ops(on)[-2:] == [["conary-flow", "relinked"], ["dev", "by-hand"]], ops(on)
        nobody_else_reads_the_key()

    def keeps_nothing(uid):
        # Nothing names it, it owns nothing, and it can neither read the
        # key nor write the ledger, by number, since its user may be gone.
        for f in [ledger] + [f"{ledger}/{f}" for f in on.succeed(f"ls -A {ledger}").split()]:
            assert f":{uid}:" not in on.succeed(f"getfacl -cpn {f}"), f
        assert on.succeed(f"find {ledger} -maxdepth 1 -uid {uid}") == ""
        as_it = f"setpriv --reuid={uid} --regid={uid} --clear-groups"
        on.fail(f"{as_it} cat {ledger}/key")
        on.fail(f"{as_it} sh -c 'echo x >>{ledger}/ops.jsonl'")
        on.fail(f"{as_it} touch {ledger}/x")

    with subtest("a controller changed or turned off keeps nothing"):
        old = on.succeed("id -u conary-flow").strip()
        # The flow renames the anchor in, so it's the flow's own.
        on.succeed("systemctl start flow@last.service")
        assert on.succeed(f"stat -c %U {ledger}/anchor").strip() == "conary-flow"
        on.succeed("/run/booted-system/specialisation/moved/bin/switch-to-configuration test")
        new = on.succeed("id -u conary-flow2").strip()
        on.succeed("systemctl start flow@moved.service")
        assert ops(on)[-1] == ["conary-flow2", "moved"], ops(on)
        keeps_nothing(old)
        on.succeed("/run/booted-system/specialisation/retired/bin/switch-to-configuration test")
        on.succeed("systemctl start burrow-ledger@sam-r.service")
        assert ops(on)[-1] == ["dev", "sam/r"], ops(on)
        keeps_nothing(old)
        keeps_nothing(new)
        assert on.succeed(f"getfacl -cps {ledger} {ledger}/*") == ""
        assert on.succeed(f"stat -c '%U %a' {ledger}/key").strip() == "dev 600"
        # Its ACL gone, etags is no wider than it was.
        assert on.succeed(f"stat -c '%U %a' {ledger}/etags").strip() == "dev 700"
        on.fail(f"runuser -u other -- ls {ledger}/etags")

    with subtest("with the option off, nothing changes"):
        off.wait_for_unit("burrow-ledger-access.service")
        off.succeed("systemctl start burrow-ledger@sam-r.service")
        off.succeed("systemctl start flow@by-dev.service")
        assert [o[0] for o in ops(off)] == ["dev", "dev"], ops(off)
        off.fail("id conary-flow")
        off.fail("test -e /run/conary-producers")
        off.fail("test -e /var/lib/burrow/flow")
        assert off.succeed(f"getfacl -cps {ledger} {ledger}/key") == ""
        assert off.succeed(f"stat -c '%U %a' {ledger}/key").strip() == "dev 600"

    # The access unit's own tests that set ACLs on files, which a Nix
    # build's seccomp filter refuses (crates/burrow-ledger-access's
    # #[ignore]d ones), with the rest, as a user, in a tmp that takes ACLs.
    with subtest("the access unit's tests that set ACLs pass"):
        print(off.succeed("for t in ${accessTests}/bin/*; do runuser -u dev -- $t --include-ignored 2>&1 || exit 1; done"))
  '';
}
