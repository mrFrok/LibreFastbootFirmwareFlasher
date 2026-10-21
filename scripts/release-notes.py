#!/usr/bin/env python3
"""Print one version's section of CHANGELOG.md, for the GitHub release body.

    python3 scripts/release-notes.py 3.0.0 > notes.md

A section starts at a `## <version>` heading (anything may follow the
version on that line, such as a date) and runs to the next `## ` heading.
Exits 1 when the version has no section or it is empty: a release without
notes is the thing this is here to prevent.
"""

import re
import sys
from pathlib import Path

CHANGELOG = Path(__file__).resolve().parent.parent / "CHANGELOG.md"


def section(text: str, version: str) -> str | None:
    version = version.lstrip("v")
    lines = text.splitlines()
    head = re.compile(rf"^## v?{re.escape(version)}(\s|$)")
    for i, line in enumerate(lines):
        if head.match(line):
            body = []
            for rest in lines[i + 1 :]:
                if rest.startswith("## "):
                    break
                body.append(rest)
            notes = "\n".join(body).strip()
            return notes or None
    return None


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: release-notes.py <version>", file=sys.stderr)
        return 2
    notes = section(CHANGELOG.read_text(), sys.argv[1])
    if notes is None:
        print(
            f"CHANGELOG.md has no notes for {sys.argv[1]}: add a '## {sys.argv[1].lstrip('v')}' section",
            file=sys.stderr,
        )
        return 1
    print(notes)
    return 0


if __name__ == "__main__":
    sys.exit(main())
