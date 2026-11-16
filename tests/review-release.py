# Included by review-producer.nix: no release depends on a shutdown path event.
with subtest("answers remain in hand until their snapshots are released"):
    import json

    worker = machine.succeed("readlink -f /etc/burrow/review/burrow-review").strip()
    home = "/var/lib/burrow/autoland-review"
    review = "/var/lib/burrow-review-looks"
    # A second oneshot command delays systemd's transition back to watching
    # paths after the real worker returns. The old final sweep leaves an
    # answer behind if its snapshot disappears within that interval.
    script = f"#!/bin/sh\n{worker} work || exit $?\ntouch {home}/after-work\nwhile [ ! -e {home}/exit-release ]; do sleep 0.1; done\n"
    machine.succeed("echo " + shlex.quote(script) + f" >{home}/release-worker; chmod 0755 {home}/release-worker")
    override = f"[Service]\nExecStart=\nExecStart={home}/release-worker\n"
    machine.succeed("echo " + shlex.quote(override) + " >/run/systemd/system/autoland-review.service.d/release.conf")
    machine.succeed("systemctl daemon-reload; systemctl unmask --runtime autoland-review.path")
    for kind in ["queue", "judge-queue"]:
        snap = f"release-{kind}"
        path = f"{review}/{kind}/codex--{snap}.json"
        answer = f"{review}/done/codex--{snap}.json"
        # Both queues produce an error answer without launching an agent.
        entry = dict(reviewer="codex", snap=snap, repo="sam/r", number=1,
                     headRefOid="a" * 40, baseRefName="main", prompt="test", description="", opened=0, at=0)
        machine.succeed("systemctl stop autoland-review.path")
        machine.succeed(f"rm -f {home}/after-work {home}/exit-release; mkdir -p {review}/snapshots/{snap}")
        machine.succeed("echo " + shlex.quote(json.dumps(entry)) + f" >{path}")
        machine.succeed("systemctl start autoland-review.path")
        machine.wait_for_file(answer)
        machine.wait_until_succeeds(f"test ! -e {path}")
        # Past an idle loop interval, the real worker must still retain its
        # answer. In the buggy version the wrapper has already reached here.
        machine.sleep(6)
        machine.succeed(f"test ! -e {home}/after-work; test -e {answer}")
        machine.succeed(f"rm -r {review}/snapshots/{snap}")
        machine.wait_until_succeeds(f"test ! -e {answer}")
        machine.wait_for_file(f"{home}/after-work")
        machine.succeed(f"touch {home}/exit-release")
        machine.wait_until_succeeds("test \"$(systemctl show -P ActiveState autoland-review.service)\" = inactive")
    # Restart cleanup needs no new request or release event: a persisted
    # answer itself keeps the path condition true after a stopped worker.
    machine.succeed("systemctl stop autoland-review.path")
    orphan = f"{review}/done/codex--released-at-restart.json"
    as_user("autoland-review", f"echo '{{}}' >{orphan}")
    machine.succeed("systemctl start autoland-review.path")
    machine.wait_until_succeeds(f"test ! -e {orphan}")
    machine.wait_until_succeeds("test \"$(systemctl show -P ActiveState autoland-review.service)\" = inactive")
    machine.succeed("systemctl stop autoland-review.path; rm /run/systemd/system/autoland-review.service.d/release.conf; systemctl daemon-reload")
