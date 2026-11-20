"""A pass waits for the latest rate-limit deadline any of its calls met:
burrow-upkeep's `limited` (the failed request's own deadline, as
crates/burrow-upkeep's pass::tests::limits pin it) is never shortened by
one met before it, nor shortens one."""
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
    config.write_text(json.dumps(dict(context='test', stateDir=tmp, upkeep=['upkeep'],
                                     conary={'cn': 'cn', 'env': {}}, trainContext='train')))
    os.environ['AUTOLAND_CONFIG'] = str(config)
    import common as C

    def said(limited):
        out = json.dumps({"failed": [], "limited": limited})
        return patch.object(C.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, out))

    with patch.object(C, 'gh_env', return_value={}):
        # A gh call's secondary limit noted first, at 0 (GitHub said no
        # reset): upkeep's later Retry-After deadline still holds the pass.
        C.LIMITED[:] = [0.0]
        with said(1800000600.0):
            C.upkeep('settle', 'sam/r')
        assert C.LIMITED == [1800000600.0], C.LIMITED
        # An earlier one after it, or none, leaves it.
        for limited in (1800000060.0, None):
            with said(limited):
                C.upkeep('settle', 'sam/r')
            assert C.LIMITED == [1800000600.0], C.LIMITED
        # A later one lengthens it.
        with said(1800000900.0):
            C.upkeep('settle', 'sam/r')
        assert C.LIMITED == [1800000900.0], C.LIMITED
        # The first a pass meets is noted as it is.
        C.LIMITED.clear()
        with said(1800000300.0):
            C.upkeep('settle', 'sam/r')
        assert C.LIMITED == [1800000300.0], C.LIMITED
print('rate-limit deadlines: passed')
