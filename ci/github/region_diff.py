# Whitespace-neutral region comparison + gate wiring for the
# negative-control workflow.  Pure python; the spans come from
# `cargo run -q -p test-spans -- <file>` (ra_ap_syntax).
#
# Regions are compared after whitespace-normalization (runs of
# whitespace collapse to one space; leading/trailing stripped) so a
# reformat-only change to a test is COSMETIC, not a rule-1 violation.

import json
import re
import subprocess


def spans_of(file, cwd=None):
    """The test spans of a file, via the test-spans tool."""
    out = subprocess.run(
        ["cargo", "run", "-q", "-p", "test-spans", "--", file],
        capture_output=True, text=True, check=True, cwd=cwd,
    ).stdout
    return json.loads(out.strip().splitlines()[-1])


def lines_of(text):
    return text.splitlines()


def region_text(text, start, end):
    """The 1-based inclusive line range [start, end] of text."""
    ls = lines_of(text)
    return "\n".join(ls[start - 1 : end])


def normalize(text):
    """Whitespace-neutral form: ALL whitespace is removed — two
    regions are 'cosmetic-equal' iff their non-whitespace character
    streams are identical (any pure-reformat diff collapses to equal;
    any token/comment change reads as a real change)."""
    return "".join(text.split())


def changed_test_regions(file, spans_a, text_a, spans_b, text_b):
    """Compare the test regions of two versions of a file by span path.

    Returns a list of (path, kind): 'changed' (whitespace-neutral
    content differs - a rule-1 violation), 'cosmetic' (only
    whitespace differs), 'added' / 'removed' (the span exists on one
    side only).  Moves with content changes read as changed; pure
    whitespace edits read as cosmetic."""
    by_a = {s["path"]: s for s in spans_a}
    by_b = {s["path"]: s for s in spans_b}
    out = []
    for path in sorted(set(by_a) | set(by_b)):
        a, b = by_a.get(path), by_b.get(path)
        if a and not b:
            out.append((path, "removed"))
            continue
        if b and not a:
            out.append((path, "added"))
            continue
        ra = region_text(text_a, a["start"], a["end"])
        rb = region_text(text_b, b["start"], b["end"])
        if normalize(ra) == normalize(rb):
            if ra == rb:
                continue  # byte-identical
            out.append((path, "cosmetic"))
        else:
            out.append((path, "changed"))
    return out
