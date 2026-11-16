# Included by review-producer.nix: real systemd groups and mount namespaces.
with subtest("drained workers receive fresh execution contexts on deploy and rollback"):
    import json

    worker = machine.succeed("readlink -f /etc/burrow/review/burrow-review").strip()
    home = "/var/lib/burrow/autoland-review"
    root = "/run/review-context"
    machine.succeed(f"systemctl stop autoland-review.service; mkdir -p {root} /run/systemd/system/autoland-review.service.d")
    config = json.loads(machine.succeed("cat /etc/burrow/review/config.json"))
    config["kick"] = "http://127.0.0.1:1"
    config["settings"]["allowed"] = ["sam/r"]
    # Each generation needs a distinct writable tree, and only the protected
    # generation needs askers membership. The reverse transition tests rollback.
    configs = {}
    for generation in ["old", "protected"]:
        cfg = dict(config)
        if generation == "old":
            cfg["producers"] = None
        configs[generation] = f"{root}/{generation}.json"
        machine.succeed("echo " + shlex.quote(json.dumps(cfg)) + f" >{configs[generation]}")

    def deploy(generation):
        machine.succeed(f"ln -sfn {configs[generation]} {root}/config.json")

    def unit(generation, legacy=False):
        # The shell fixture models the previous binary's final drain + exec.
        # It cannot receive new groups or mounts from an exec.
        command = f"{home}/legacy" if legacy else f"{worker} work"
        paths = f"{home} /var/lib/burrow/autoland/review/queue /var/lib/burrow/autoland/review/done /var/lib/burrow/autoland/review/sessions"
        if generation == "protected":
            paths += " /var/lib/burrow-review-looks"
        text = "\n".join([
            "[Service]", "ExecStart=", f"ExecStart={command}",
            f"Environment=REVIEW_CONFIG={configs[generation]}",
            f"Environment=REVIEW_DEPLOYED={root}",
            f"Environment=REVIEW_CONTEXT={'legacy' if legacy else configs[generation]}",
            "ReadWritePaths=", f"ReadWritePaths={paths}",
        ]) + "\n"
        machine.succeed("echo " + shlex.quote(text) + " >/run/systemd/system/autoland-review.service.d/context.conf")

    def request(generation, snap, parked=False):
        queue = "/var/lib/burrow-review-looks/queue" if generation == "protected" else "/var/lib/burrow/autoland/review/queue"
        done = "/var/lib/burrow-review-looks/done" if generation == "protected" else "/var/lib/burrow/autoland/review/done"
        entry = dict(reviewer="codex", snap=snap, repo="sam/parked" if parked else "sam/r",
                     number=1, headRefOid="a" * 40, baseRefName="main", prompt="test", description="", opened=0, at=0)
        # Missing execution options produces an error without starting an agent.
        # The protected request is producer-owned and 0640: it needs the group.
        if generation == "protected":
            machine.succeed(f"mkdir -p /var/lib/burrow-review-looks/snapshots/{snap}")
        path = f"{queue}/codex--{snap}.json"
        machine.succeed("echo " + shlex.quote(json.dumps(entry)) + f" >{path}")
        owner = "autoland-review-producer:autoland-review-askers" if generation == "protected" else "dev:autoland-review"
        machine.succeed(f"chown {owner} {path}; chmod 0640 {path}")
        return path, f"{done}/codex--{snap}.json"

    # Retained active legacy service: complete its in-flight operation before
    # entering the new binary, which must refuse the inherited execution context.
    deploy("protected")
    script = f"#!/bin/sh\ntouch {home}/underway\nwhile [ ! -e {home}/release ]; do sleep 0.1; done\ntouch {home}/completed\nexport REVIEW_CONFIG={configs['protected']}\nexec {worker} work\n"
    machine.succeed("echo " + shlex.quote(script) + f" >{home}/legacy; chmod 0755 {home}/legacy; usermod -G '' autoland-review")
    unit("old", legacy=True)
    machine.succeed("systemctl daemon-reload; systemctl start --no-block autoland-review.service")
    machine.wait_for_file(f"{home}/underway")
    path, answer = request("protected", "context-active")
    machine.succeed("usermod -a -G autoland-review-askers autoland-review")
    unit("protected")
    # Release before daemon-reload: the loaded unit is still the old one.
    machine.succeed(f"touch {home}/release")
    machine.wait_until_succeeds("journalctl -u autoland-review.service --no-pager | grep 'execution context is stale'")
    machine.succeed(f"test -e {path}; test ! -e {answer}; test -e {home}/completed")
    machine.succeed("systemctl daemon-reload")
    machine.wait_until_succeeds(f"test -e {answer}")
    machine.succeed(f"test ! -e {path}")
    result = json.loads(machine.succeed(f"cat {answer}"))
    assert "execution options" in result["error"], result
    assert len(result["digest"]) == 64, result
    machine.succeed("rm -r /var/lib/burrow-review-looks/snapshots/context-active")

    # A real parked worker, then starts from a not-yet-reloaded unit, in
    # both directions. The queued request and snapshot survive each drain.
    for source, target in [("protected", "old"), ("old", "protected")]:
        machine.wait_until_succeeds("test \"$(systemctl show -P ActiveState autoland-review.service)\" = inactive")
        deploy(source)
        unit(source)
        machine.succeed("systemctl daemon-reload; systemctl start --no-block autoland-review.service")
        parked_path, _ = request(source, f"context-parked-{source}", parked=True)
        machine.succeed("systemctl start --no-block autoland-review.service")
        machine.wait_until_succeeds("test \"$(systemctl show -P ActiveState autoland-review.service)\" = activating")
        deploy(target)
        # Before loading the target unit, the old one may start again. It
        # yields without consuming the target's producer-owned request.
        path, answer = request(target, f"context-{target}")
        machine.wait_until_succeeds("test \"$(systemctl show -P SubState autoland-review.service)\" = auto-restart")
        machine.succeed(f"test -e {parked_path}; test -e {path}; test ! -e {answer}")
        unit(target)
        machine.succeed("systemctl daemon-reload")
        machine.wait_until_succeeds(f"test -e {answer}")
        machine.succeed(f"test ! -e {path}; rm {parked_path}")
        if target == "protected":
            machine.succeed("rm -r /var/lib/burrow-review-looks/snapshots/context-protected")

    machine.succeed("systemctl stop autoland-review.service; rm /run/systemd/system/autoland-review.service.d/context.conf; systemctl daemon-reload")
