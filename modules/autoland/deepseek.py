"""DeepSeek, the steward's second reviewer: what a pass reads of its
verdicts, and the service (`autoland-steward deepseek`) that works through the
queued reviews, sends each head's diff to DeepSeek's API and posts what it
says as one review."""

import json
import os
import re
import ssl
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

import common as C
from common import CFG, DS, STATE, load, log, repo_cfg, save, ts

# The steward posts DeepSeek's reviews under the user's own login, so they're
# told apart by these markers: the review's names the commit and how many
# findings it had, and each finding's thread carries the other.
DS_RE = re.compile(r"<!-- deepseek-review:v1 (\{.*?\}) -->")
DS_FINDING = "<!-- deepseek-finding -->"
QUEUE, DONE = "ds-queue", "ds-done"  # under STATE: asked for, and what went wrong


def deepseek_verdict(pr, head):
    """What DeepSeek last said about this exact head (clean, findings or
    none), when, and how many of its reviews on the PR had findings."""
    verdict, at, rounds = "none", 0, 0
    for r in pr["reviews"]["nodes"]:
        m = DS_RE.search(r["body"] or "")
        if not m or (r["author"] or {}).get("login") != C.me():
            continue
        try:
            found = int(json.loads(m.group(1)).get("findings", 0)) > 0
            sha = json.loads(m.group(1)).get("commit_sha")
        except (ValueError, TypeError, AttributeError):
            continue
        rounds += found
        if sha == head and (r["commit"] or {}).get("oid") == head and ts(r["submittedAt"]) >= at:
            verdict, at = ("findings" if found else "clean"), ts(r["submittedAt"])
    return verdict, at, rounds


QUOTED = r'"(?:[^"\\]|\\.)*"'


def unquote(t):
    """A name as git writes it: bare, or quoted with C-style escapes (the
    octal ones bytes of UTF-8) when it holds unusual characters. None if
    it's neither."""
    if not t.startswith('"'):
        return t
    if not re.fullmatch(QUOTED, t):
        return None
    try:
        return t[1:-1].encode("latin-1", "backslashreplace").decode("unicode_escape").encode("latin-1").decode("utf-8", "replace")
    except (UnicodeError, ValueError):
        return None


def header_pairs(header):
    """Every (old, new) a `diff --git` line can be read as. Git leaves
    spaces bare, so "a/x b/y b/z" is two renames at once; the caller
    checks them all."""
    rest = header[len("diff --git "):]
    pairs = []
    for i, c in enumerate(rest):
        if c == " ":
            a, b = unquote(rest[:i]), unquote(rest[i + 1:])
            if a and b and a.startswith("a/") and b.startswith("b/"):
                pairs.append((a[2:], b[2:]))
    return pairs


def file_names(lines):
    """For one file's part of a diff: every name it might have (old or
    new, however its header splits), and the one it has on the new side
    (the old one if it's deleted), or None if that's not certain."""
    pairs = header_pairs(lines[0])
    names = {x for p in pairs for x in p}
    told = {}
    for x in lines[1:]:
        if x.startswith("@@"):
            break
        m = re.match(r"(rename|copy) (from|to) (.*)", x) or re.match(r"(---|\+\+\+) (?:(/dev/null)|(.*?))\t?$", x)
        if not m:
            continue
        if m.group(2) == "/dev/null":
            told[m.group(1)] = None
            continue
        name = unquote(m.group(3))
        if m.group(1) in ("---", "+++"):
            name = name[2:] if name and name[:2] in ("a/", "b/") else None
        if name is None:
            return names, None  # a name it can't read
        names.add(name)
        told[m.group(1) + (m.group(2) if m.group(1) in ("rename", "copy") else "")] = name
    same = {b for a, b in pairs if a == b}
    path = (told.get("+++") or told.get("renameto") or told.get("copyto")
            or (told.get("---") if "+++" in told else None)
            or (same.pop() if len(same) == 1 else pairs[0][1] if len(pairs) == 1 else None))
    return names, path


def numbered(diff):
    """The diff, file by file, with each new-side line's number in front (the
    numbers findings point at), and the lines a review comment can go on."""
    files, out = {}, []
    skip = [re.compile(p) for p in DS["skipPaths"]]
    for part in re.split(r"(?m)^(?=diff --git )", diff):
        lines = part.splitlines()
        if not lines or not lines[0].startswith("diff --git "):
            continue
        names, path = file_names(lines)
        # A rename out of a secrets path still carries its contents, and a
        # file whose name isn't certain is hidden rather than risked.
        hidden = path is None or any(p.search(q) for p in skip for q in names)
        path = path or lines[0]
        files[path] = set()
        out.append(f"\n=== {path}" + ("\n(contents not sent: a secrets path)" if hidden else ""))
        if hidden:
            continue
        new, in_hunk = 0, False
        for x in lines[1:]:
            if x.startswith("@@"):
                m = re.match(r"@@ -\d+(?:,\d+)? \+(\d+)", x)
                new, in_hunk = (int(m.group(1)) if m else 0), True
                out.append(x)
            elif not in_hunk:
                # The file's header: only the lines that say what happened to it.
                if x.startswith(("new file", "deleted file", "old mode", "new mode", "rename ", "copy ", "Binary files")):
                    out.append(x)
            elif x.startswith("-"):
                out.append(f"{'':>6} {x}")
            elif x.startswith(("+", " ")):
                files[path].add(new)
                out.append(f"{new:>6} {x}")
                new += 1
            else:
                out.append(f"{'':>6} {x}")  # "\ No newline at end of file"
    return "\n".join(out), files


def review_rules(repo, base, paths):
    """The `## Code Review Rules` of the AGENTS.md nearest each changed file,
    as the base branch has them (a PR can't rewrite its own rules)."""
    tree = C.gh("api", f"repos/{repo}/git/trees/{base}?recursive=1", "--jq",
                '[.tree[] | select(.type == "blob") | .path | select(test("(^|/)AGENTS\\\\.md$"))]', check=False)
    have = set(tree) if isinstance(tree, list) else set()
    wanted = set()
    for p in paths:
        d = p
        while d:
            d = d.rpartition("/")[0]
            f = f"{d}/AGENTS.md" if d else "AGENTS.md"
            if f in have:
                wanted.add(f)
                break
    rules = []
    for f in sorted(wanted):
        text = C.gh("api", "-H", "Accept: application/vnd.github.raw", f"repos/{repo}/contents/{f}?ref={base}", check=False)
        m = re.search(r"^## Code Review Rules\s*$(.*?)(?=^## |\Z)", text if isinstance(text, str) else "", re.M | re.S)
        if m:
            rules.append(f"From {f}:\n{m.group(1).strip()}")
    return "\n\n".join(rules)


def prior_findings(repo, n):
    """DeepSeek's earlier findings on this PR and the replies each got."""
    out = C.gh("api", "--paginate", f"repos/{repo}/pulls/{n}/comments?per_page=100", "--jq",
               ".[] | {id, in_reply_to_id, path, line, original_line, body, user: .user.login}", check=False)
    cs = []
    for x in (out if isinstance(out, str) else json.dumps(out) if out else "").splitlines():
        try:
            cs.append(json.loads(x))
        except ValueError:
            pass
    roots = [c for c in cs if c["user"] == C.me() and DS_FINDING in (c["body"] or "")]
    text = []
    for c in roots:
        # Only replies from those whose PRs it reviews: anyone else's text
        # never steers it.
        replies = [r for r in cs if r.get("in_reply_to_id") == c["id"]
                   and (r["user"] in CFG["authors"] or r["user"] == C.me())]
        body = c["body"].replace(DS_FINDING, "").strip()
        at = c.get("line") or c.get("original_line")
        text.append(f"- {c['path']}{f':{at}' if at else ''}: {body}\n"
                    + "".join(f"  Reply from {r['user']}: {r['body'].strip()}\n" for r in replies))
    return "\n".join(text)


# The answer's shape. In strict mode (DeepSeek's beta endpoint) the API
# holds the model to it: every property required, nothing else allowed.
DS_REVIEW_SCHEMA = {
    "type": "object",
    "properties": {
        "findings": {"type": "array", "items": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "line": {"type": "integer"},
                "severity": {"type": "string", "enum": ["P0", "P1", "P2"]},
                "title": {"type": "string"},
                "body": {"type": "string"},
            },
            "required": ["path", "line", "severity", "title", "body"],
            "additionalProperties": False,
        }},
        "summary": {"type": "string"},
    },
    "required": ["findings", "summary"],
    "additionalProperties": False,
}


def ask_deepseek(system, user):
    """One review from DeepSeek, as a dict. Raises on any failure.

    First as a strict tool call, which the API holds to DS_REVIEW_SCHEMA; if
    that's turned down (it's a beta), JSON mode, which now and then answers
    with something that isn't JSON (its docs say so, and remi saw one in
    six); if that's turned down too, plain text. Each answer that can't be
    read is asked again, three tries in all.

    Its thinking counts against max_tokens. At max effort a big diff can
    think past even that (remi#39: most calls did, after 11 minutes each),
    and asking the same again would only do it again: it's asked once more
    at high effort, and if that runs out too the review fails."""
    env = {}
    for x in open(os.path.join(os.environ.get("CREDENTIALS_DIRECTORY", ""), "deepseek")):
        k, sep, v = x.strip().partition("=")
        if sep:
            env[k] = v.strip().strip('"')
    efforts = [env.get("DEEPSEEK_EFFORT") or DS["effort"]]
    if efforts[0] == "max":
        efforts.append("high")
    base = {
        "model": env["DEEPSEEK_MODEL"],
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
        "max_tokens": DS["maxTokens"],
    }
    url = DS["baseUrl"].rstrip("/")
    ways = [
        (url + "/beta", {
            "tools": [{"type": "function", "function": {
                "name": "submit_review", "strict": True,
                "description": "Submit the review: every finding, and a one-sentence summary.",
                "parameters": DS_REVIEW_SCHEMA}}],
            # Thinking mode turns down a forced or "required" tool_choice
            # (remi#34); with "auto" it called submit_review every time.
            "tool_choice": "auto",
        }),
        (url, {"response_format": {"type": "json_object"}}),
        (url, {}),
    ]
    ctx = ssl.create_default_context(cafile=DS["caFile"])

    def call(at, extra):
        body = {**base, **extra, "reasoning_effort": efforts[0]}
        req = urllib.request.Request(at + "/chat/completions", data=json.dumps(body).encode(),
                                     headers={"Authorization": f"Bearer {env['DEEPSEEK_API_KEY']}",
                                              "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=DS["timeout"], context=ctx) as r:
            choice = json.load(r)["choices"][0]
        msg = choice["message"]
        for t in msg.get("tool_calls") or []:
            if t["function"]["name"] == "submit_review":
                return choice.get("finish_reason"), t["function"]["arguments"] or ""
        return choice.get("finish_reason"), msg.get("content") or ""

    tries = 0
    while True:
        at, extra = ways[0]
        try:
            why, text = call(at, extra)
        except urllib.error.HTTPError as e:
            if e.code != 400 or len(ways) == 1:
                raise
            log(f"  DeepSeek turned down {', '.join(extra) or 'plain text'} ({e.code}): trying the next way")
            ways.pop(0)
            continue
        if why == "length":
            if len(efforts) == 1:
                raise RuntimeError(f"thought past {DS['maxTokens']} tokens at {efforts[0]} effort without answering")
            log(f"  DeepSeek thought past {DS['maxTokens']} tokens at {efforts[0]} effort: asking at {efforts[1]}")
            efforts.pop(0)
            continue
        tries += 1
        try:
            return json.loads(text[text.index("{"):text.rindex("}") + 1])
        except ValueError:
            if tries == 3:
                raise RuntimeError(f"no JSON in its answer: {text[:100]!r}")


DS_SYSTEM = """You review pull requests for {owner}'s repos. You are one of two reviewers; a PR merges itself once both find nothing on its latest commit, so what you let through ships.

Report only real defects a careful senior reviewer would block the merge for: bugs, security holes, data loss, races, broken error handling, tests or checks weakened to get green, behaviour changed beyond what the PR says it does, and anything the repo's Code Review Rules flag. No style or naming nits, no praise, no "consider" suggestions, no restating the diff, nothing you can't point at in the code. If nothing qualifies, return no findings: that is the usual outcome for a good commit.

Earlier findings on this PR are listed with the replies they got. Don't raise one again unless the code still has the problem and the reply is wrong; then say why the reply doesn't hold.

Each finding: "path" (as the diff names it), "line" (a number from the diff's left column, on the new side, where the problem is), "severity" ("P0": unsafe or breaks things, "P1": a real bug or a rule broken, "P2": a smaller real defect), "title" (one line), "body" (what is wrong, why, and the fix, in a few sentences). Most severe first.

Answer by calling submit_review if you have it, otherwise with JSON only: {{"findings": [...], "summary": "one sentence on the PR as a whole"}}"""


def quiet_mentions(text):
    """Model text goes out under the user's login: no @-pings from it."""
    return re.sub(r"@(?=[\w-])", "@​", str(text)).replace("<!--", "&lt;!--")


def run_deepseek(repo, pr):
    """Ask DeepSeek about the head and post what it says as one review.
    Returns "clean", "findings", or "tooBig" (nothing sent). Raises if it
    didn't happen."""
    n, head = pr["number"], pr["headRefOid"]
    diff = C.gh("api", "-H", "Accept: application/vnd.github.diff",
                f"repos/{repo}/compare/{pr['baseRefName']}...{head}", check=False)
    if not isinstance(diff, str) or not diff.startswith("diff"):
        raise RuntimeError("couldn't read the diff")
    text, files = numbered(diff)
    if len(text) > DS["maxDiffChars"]:
        return "tooBig"
    user = (f"Repo {repo}, PR #{n}: {pr['title']}\n\n"
            f"Code Review Rules:\n{review_rules(repo, pr['baseRefName'], files) or '(none)'}\n\n"
            f"Earlier findings:\n{prior_findings(repo, n) or '(none)'}\n\n"
            f"The diff against {pr['baseRefName']}:\n{text}")
    log(f"{repo}#{n}: asking DeepSeek ({len(text)} chars of diff)")
    answer = ask_deepseek(DS_SYSTEM.format(owner=CFG["owner"]), user)
    # Anything but a list of findings is no review at all, never a clean one.
    found = answer.get("findings") if isinstance(answer, dict) else None
    if not isinstance(found, list) or not all(isinstance(f, dict) for f in found):
        raise RuntimeError(f"not a review: {json.dumps(answer)[:100]}")
    comments, whole_file, loose = [], [], []
    for f in found:
        sev = f.get("severity") if f.get("severity") in ("P0", "P1", "P2") else "P1"
        msg = f"**DeepSeek · {sev}** {quiet_mentions(f.get('title', ''))}\n\n{quiet_mentions(f.get('body', ''))}"
        lines = files.get(f.get("path"))
        if lines is None:
            # Not a file this PR changes: a note in the review, not a finding.
            loose.append(f"- `{quiet_mentions(f.get('path'))}`: " + msg.replace("\n\n", " "))
            continue
        if not lines:
            # Changed, but with no new side to comment on (deleted, binary,
            # a secrets path): a thread on the whole file, still a finding.
            whole_file.append({"path": f["path"], "body": msg + "\n\n" + DS_FINDING})
            continue
        try:
            want = int(f.get("line"))
        except (TypeError, ValueError):
            want = min(lines)
        line = want if want in lines else min(lines, key=lambda x: abs(x - want))
        comments.append({"path": f["path"], "line": line, "side": "RIGHT", "body": msg + "\n\n" + DS_FINDING})
    k = len(comments) + len(whole_file)
    head_line = (f"**DeepSeek review** of {head[:7]}: "
                 + (f"{k} finding{'s' * (k > 1)}. Fix or answer each, then resolve its thread." if k else "nothing found."))
    body = head_line + (f"\n\n{quiet_mentions(answer.get('summary', ''))}" if answer.get("summary") else "")
    if loose:
        body += "\n\nNotes not on a changed line:\n" + "\n".join(loose)
    body += "\n\n<!-- deepseek-review:v1 " + json.dumps({"commit_sha": head, "findings": k}) + " -->"
    posted = C.gh("api", "-X", "POST", f"repos/{repo}/pulls/{n}/reviews", "--input", "-",
                  input=json.dumps({"commit_id": head, "event": "COMMENT", "body": body, "comments": comments}), check=False)
    if not isinstance(posted, dict):
        raise RuntimeError("GitHub didn't take the review")
    # A review can't hold file-level comments: they go on their own. The
    # review already counts them, so a lost one still keeps the PR unsettled.
    for c in whole_file:
        if not isinstance(C.gh("api", "-X", "POST", f"repos/{repo}/pulls/{n}/comments", "--input", "-",
                               input=json.dumps({**c, "commit_id": head, "subject_type": "file"}), check=False), dict):
            log(f"{repo}#{n}: couldn't post DeepSeek's finding on {c['path']}")
    log(f"{repo}#{n}: DeepSeek review posted, {k} finding(s)")
    return "findings" if k else "clean"


# ---------------------------------------------------------------- DeepSeek's queue

def review_queued(name):
    """One queued review: posted, or what went wrong left in DONE for the
    next pass. Gone from the queue either way."""
    item = load(f"{QUEUE}/{name}", None)
    repo, n = (item or {}).get("repo"), (item or {}).get("number")
    try:
        if not item:
            return
        # Queued before the repo was taken off DeepSeek: none of it goes.
        if not repo_cfg(repo)["deepseek"]:
            log(f"{repo}#{n}: the repo is off DeepSeek now, not reviewing it")
            return
        cur = C.gh("api", f"repos/{repo}/pulls/{n}", "--jq", "{state, sha: .head.sha}")
        if cur.get("state") != "open" or cur.get("sha") != item["headRefOid"]:
            log(f"{repo}#{n}: moved on from {item['headRefOid'][:7]}, not reviewing it")
            return
        if run_deepseek(repo, item) == "tooBig":
            save(f"{DONE}/{name}", {"result": "tooBig", "at": time.time()})
    except Exception as e:
        log(f"{repo}#{n}: DeepSeek review failed: {str(e)[:300]}")
        save(f"{DONE}/{name}", {"result": "error", "error": str(e)[:300], "at": time.time()})
    finally:
        try:
            os.remove(os.path.join(STATE, QUEUE, name))
        except OSError:
            pass


def deepseek_queue():
    """Every queued review, `parallel` at a time, until the queue is empty
    (passes may add to it meanwhile)."""
    q = os.path.join(STATE, QUEUE)
    # What went wrong on a head no pass came back for (it moved on).
    for name in os.listdir(os.path.join(STATE, DONE)):
        path = os.path.join(STATE, DONE, name)
        if time.time() - os.path.getmtime(path) > 86400:
            os.remove(path)
    with ThreadPoolExecutor(DS["parallel"]) as pool:
        busy, seen = [], set()
        while True:
            for name in sorted(os.listdir(q)):
                if name.endswith(".json") and name not in seen:
                    seen.add(name)
                    busy.append(pool.submit(review_queued, name))
            busy = [f for f in busy if not f.done()]
            if not busy:
                return
            time.sleep(5)
