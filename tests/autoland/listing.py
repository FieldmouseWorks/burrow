"""A repo's PR listing gets through however heavy its pages are: a page
GitHub gives up on is asked for again with half as many PRs, so a busy repo
gets smaller pages, never no pass. (On remi on 2026-11-13 burrow's listing of
56 open PRs came back "unexpected end of JSON input" and HTTP 504, and its
pass failed whole.)
Run: python3 tests/autoland/listing.py
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch

sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else
                str(Path(__file__).resolve().parents[2] / 'modules/autoland'))
with tempfile.TemporaryDirectory() as tmp:
    config = Path(tmp) / 'config.json'
    config.write_text(json.dumps(dict(context='test', stateDir=tmp, snapDir=tmp,
                                     conary={'cn': 'cn', 'env': {}}, trainContext='train')))
    os.environ['AUTOLAND_CONFIG'] = str(config)
    import common as C
    import prs as P

    OPEN = [{'number': n} for n in range(1, 21)]

    def github(most, why):
        """GitHub, with 20 open PRs, giving up (`why`) on a page of more
        than `most`; what each call asked for is kept in `asked`."""
        asked = []

        def graphql(query, **v):
            asked.append(v['first'])
            assert '$first: Int!' in query and 'first: $first' in query, 'the page size must reach the query'
            if v['first'] > most:
                raise C.Unavailable(f'gh api graphql -f: {why}')
            at = int(v['cursor'] or 0)
            end = min(at + v['first'], len(OPEN))
            return {'repository': {'squashMergeAllowed': True, 'pullRequests': {
                'pageInfo': {'hasNextPage': end < len(OPEN), 'endCursor': str(end)}, 'nodes': OPEN[at:end]}}}
        return graphql, asked

    def listed(most, why):
        graphql, asked = github(most, why)
        with patch.object(C, 'graphql', graphql), patch.object(C, 'acted_on', lambda pr: False), \
                patch.object(P, 'log', lambda *a: None):
            info, prs = P.open_prs('sam', 'r')
        return info, prs, asked

    # Each way GitHub gives up on a heavy page: every PR is still listed,
    # once and in order, and the smaller size is kept for the pages after.
    for why in ('HTTP 504', 'gh: HTTP 502: Bad Gateway', 'unexpected end of JSON input',
                'Something went wrong while executing your query'):
        info, prs, asked = listed(7, why)
        assert prs == OPEN, (why, prs)
        assert info == {'squashMergeAllowed': True}, info
        assert asked == [30, 15, 7, 7, 7], (why, asked)

    # A light repo's listing is one call of a whole page, as before.
    assert listed(30, 'HTTP 504')[2] == [30]

    # What asking for less can't help isn't asked again: a rate limit, a
    # network that's down. The pass defers as it always did.
    for why in ('API rate limit exceeded', 'error connecting to api.github.com', 'timed out after 120s'):
        try:
            listed(7, why)
        except C.Unavailable as error:
            assert why in str(error), error
        else:
            raise AssertionError(f'{why}: must defer, not shrink')
    graphql, asked = github(7, 'API rate limit exceeded')
    with patch.object(C, 'graphql', graphql):
        try:
            P.open_prs('sam', 'r')
        except C.Unavailable:
            pass
    assert asked == [30], asked

    # One PR GitHub can't list alone ends the shrinking: the pass defers.
    graphql, asked = github(0, 'HTTP 504')
    with patch.object(C, 'graphql', graphql), patch.object(P, 'log', lambda *a: None):
        try:
            P.open_prs('sam', 'r')
        except C.Unavailable:
            pass
        else:
            raise AssertionError('a page of one that fails must raise')
    assert asked == [30, 15, 7, 3, 1], asked

    # An answer cut off part way is GitHub not answering, for every call:
    # the pass defers and tries again; it isn't counted as a failed pass.
    cut = subprocess.CompletedProcess([], 1, stdout='', stderr='unexpected end of JSON input')
    with patch.object(C, 'paused', return_value=False), patch.object(C, 'gh_env', return_value={}), \
            patch.object(C.subprocess, 'run', return_value=cut):
        try:
            C.gh('api', 'graphql')
        except C.Unavailable:
            pass
        else:
            raise AssertionError('a cut-off answer must raise Unavailable')
