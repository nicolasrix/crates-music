"""Make `gen_config` importable when running `pytest` from this directory.

The script lives at `docker/gateway/gen_config.py`; tests live in
`docker/gateway/tests/`. We prepend the parent directory to `sys.path`
so the script can be imported as a plain module without packaging.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
