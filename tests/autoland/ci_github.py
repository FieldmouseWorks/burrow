"""HTTP replies for the real Rust CI command, from harness-owned fixtures.
This stands in for GitHub's transport; it contains no CI verdict policy.
"""

import json
import os
import sys
import urllib.parse

fixture = os.environ["CI_FIXTURE"]
with open(fixture) as f:
    data = json.load(f)
method, path = sys.argv[4:6]
if data["app"]:
    assert os.environ.get("GH_TOKEN") == data["app"], "CI lost the App identity"
data["asked"].append([method, path])
headers = ""
status = 200
if "/check-runs/" in path:
    id = path.split("/")[-2]
    got = data["annotations"].get(id, [])
    if isinstance(got, dict):
        status, body = got["status"], {"message": got["error"]}
    else:
        page = int(urllib.parse.parse_qs(path.partition("?")[2]).get("page", ["1"])[0])
        body = [{"message": m} for m in got[(page - 1) * 100:page * 100]]
        if len(got) > page * 100:
            headers = f'Link: <https://api.github.com/{path.partition("?")[0]}?per_page=100&page={page + 1}>; rel="next"\r\n'
elif "/actions/runs/" in path:
    key = path.split("/actions/runs/", 1)[1].split("/")[0]
    info = data["runs"].get(key, {"run_attempt": 1, "status": "completed", "conclusion": "cancelled"})
    if method == "POST":
        got = data["failures"].get(key)
        if got:
            status, body = got["status"], {"message": got["error"]}
        else:
            assert path.endswith(("/rerun-failed-jobs", "/rerun")), path
            data["reruns"].append(key)
            data["runs"][key] = {"run_attempt": info["run_attempt"] + 1, "status": "queued", "conclusion": None}
            status, body = 201, {}
    else:
        body = info
elif "/compare/" in path:
    base = urllib.parse.unquote(path.split("/compare/", 1)[1].split("...", 1)[0])
    fork = data["forks"].pop(0) if data["forks"] else data["fork"].get(base, data["fork"]["v"])
    body = {"merge_base_commit": {"sha": fork}}
else:
    raise AssertionError((method, path))
with open(fixture, "w") as f:
    json.dump(data, f)
sys.stdout.write(f"HTTP/1.1 {status} Reply\r\n{headers}\r\n" + json.dumps(body))
