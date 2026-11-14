"""A read of a repo's PRs that fails once must not start its traversal over.

A repo with more PRs than a slice sees to keeps a cursor between slices. When
GitHub fails the read between two of them (a 504 on the one query for every
open PR), the pass that retries it has to go on with the PRs no slice has
visited yet: starting over at the first PR each time left the last ones
without a look for as long as the read kept failing now and then."""
import copy
from contextlib import ExitStack
import json
import os
from pathlib import Path
import sys
import tempfile
from unittest.mock import patch

sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else
                str(Path(__file__).resolve().parents[2] / 'modules/autoland'))
with tempfile.TemporaryDirectory() as tmp:
    cfg = Path(tmp) / 'config.json'
    cfg.write_text(json.dumps(dict(
        context='test', stateDir=tmp, snapDir=tmp,
        conary={'cn': 'cn', 'env': {}}, trainContext='train', baselineJobs=False,
        labels={'first': 'first', 'waiting': {'ci': {'name': 'waiting: CI'}}},
        watch={'kick': 'watchdog', 'escalated': f'{tmp}/escalated.json'},
        defaults={'stallAfter': 120}, repos={},
    )))
    os.environ['AUTOLAND_CONFIG'] = str(cfg)
    import common as C
    import steward as S
    import waits as W

    repos = [{'repo': 'sam/b', 'default': 'main'}]
    info = {'defaultBranchRef': {'name': 'main', 'target': {'oid': 'base'}}}

    def pr(n):
        return dict(number=n, title=str(n), headRefOid=f'head-{n}', headRefName=f'branch-{n}',
                    baseRefName='main', labels={'nodes': []}, url=f'https://github.com/sam/b/pull/{n}')

    prs = [pr(n) for n in range(1, 8)]
    clock, handled, reads = [0], [], []

    def open_prs(owner, name):
        reads.append(C.NOW)
        if len(reads) == 2:
            raise RuntimeError('gh api graphql -f: gh: HTTP 504')
        return info, copy.deepcopy(prs)

    def handle(repo, pr, info, mine, *args):
        handled.append(pr['number'])
        clock[0] += 200  # three a slice
        W.NOTED[repo][W.key_of(repo, pr)] = W.item(repo, pr, 'CI', 120, f'autoland/{repo}', pr['headRefOid'])

    def coverage(repos, covered):
        done = {r['repo'] for r in covered}
        return {'at': C.NOW, 'repos': {r['repo']: {'covered': r['repo'] in done} for r in repos}}

    with ExitStack() as stack:
        for obj, name, value in (
            (S.R, 'discover', lambda: repos), (S.R, 'restart', lambda *a: None),
            (S.R, 'settle_repo', lambda *a: False), (S.R, 'watch_default', lambda *a: {}),
            (S.R, 'publish', lambda rows: None), (S.R, 'hooks', lambda *a: None),
            (S, 'open_prs', open_prs),
            (S.P, 'begin', lambda *a: None), (S.P, 'place', lambda repo, n: n),
            (S.T, 'begin', lambda repo: S.T.STOPPED.pop(repo, None)), (S.T, 'on', lambda *a: False),
            (S.step, 'handle', handle),
            (S, 'branches', lambda repo, *a, **k: None),
            (S, 'upkeep', lambda *a: None), (S.V, 'sweep', lambda *a: None),
            (C, 'follow_renames', lambda names: None), (C, 'taken', lambda *a: None),
            (C, 'coverage', coverage), (C, 'poke', lambda *a: None),
        ):
            stack.enter_context(patch.object(obj, name, value))
        stack.enter_context(patch('time.monotonic', side_effect=lambda: clock[0]))
        C.NOW = 1000

        def look(kick=None, continuing=False):
            C.WAKE.clear()
            S.run_pass(kick, continuing=continuing)
            C.NOW += 601
            clock[0] += 601

        # The first slice sees to three of the seven and yields.
        look()
        assert handled == [1, 2, 3], handled
        # GitHub fails the next read. The repo's wait says so, and it is
        # looked at again five minutes on.
        look(continuing=True)
        assert handled == [1, 2, 3], handled
        failing = C.load('watch.json', {})['items']['sam/b']
        assert failing['phase'] == W.FAILS, failing
        assert C.load('wake.json', {})['sam/b'] == C.NOW - 601 + 300, C.load('wake.json', {})
        # That look is the repo's own, as the wake-up makes it: it goes on
        # with the PRs no slice has visited, and the ones it revisits (new
        # work invalidates what was decided) come after them.
        look(kick=['sam/b'])
        assert handled[3:6] == [4, 5, 6], ('the retry started the traversal over', handled)
        look(kick=['sam/b'], continuing=True)
        assert handled[6] == 7, ('the last PR waited behind ones already seen to', handled)
print('a failed read keeps the traversal: passed')
