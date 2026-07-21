"""hermes_kawarimi — import Hermes agents into LunarWing tenants.

A "Kawarimi adapter": reads a Hermes agent's on-disk footprint
(``$HERMES_HOME``: markdown memory, ``state.db``, ``config.yaml``, credentials)
and imports it into a freshly-provisioned LunarWing tenant via
provision-then-populate.

See ``README.md`` for the mapping and usage.
"""

from __future__ import annotations

__version__ = "0.1.0"
