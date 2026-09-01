#!/usr/bin/env python3
"""Summarise the `dhat-heap.json` written by `heap_profile` by allocation site at peak.

Usage:
    cargo run --release --example heap_profile 5000 table
    python3 core/examples/heap_report.py dhat-heap.json

dhat's JSON records, for each allocation site (program point), its call stack and
the "bytes still live at peak" (tgb). This regroups those by the frames belonging to
our own code and prints them largest first.
"""
import json
import re
import sys
from collections import defaultdict

# Only frames from our own code are used as headings (never cut inside the standard library).
OWN = re.compile(r'sghtmltopdf_core::([\w:]+)')


def main(path: str, top: int = 15) -> None:
    data = json.loads(open(path).read())
    frames = data['ftbl']
    total_peak = 0
    by_site = defaultdict(lambda: [0, 0])  # heading -> [bytes, block count]

    for pp in data['pps']:
        peak_bytes = pp.get('gb', 0)
        if not peak_bytes:
            continue
        total_peak += peak_bytes
        # Walk the stack bottom-up and use the first frame of our own code as the heading.
        label = '(no frame of our own)'
        for index in pp['fs']:
            found = OWN.search(frames[index])
            if found:
                label = found.group(1)
                break
        entry = by_site[label]
        entry[0] += peak_bytes
        entry[1] += pp.get('gbk', 0)

    print(f'total at peak: {total_peak / 1024 / 1024:.1f}MB\n')
    print(f'{"allocation site":<52} {"MB":>8} {"count":>10}')
    ranked = sorted(by_site.items(), key=lambda kv: kv[1][0], reverse=True)
    for label, (size, blocks) in ranked[:top]:
        print(f'{label:<52} {size / 1024 / 1024:>8.1f} {blocks:>10,}')


def detail(path: str, needle: str, top: int = 8) -> None:
    """Print allocation sites whose stack contains `needle`, ordered by bytes at peak."""
    data = json.loads(open(path).read())
    frames = data['ftbl']
    rows = []
    for pp in data['pps']:
        peak_bytes = pp.get('gb', 0)
        stack = [frames[i] for i in pp['fs']]
        if not peak_bytes or not any(needle in f for f in stack):
            continue
        rows.append((peak_bytes, pp.get('gbk', 0), stack))
    rows.sort(reverse=True, key=lambda r: r[0])
    for size, blocks, stack in rows[:top]:
        avg = size / blocks if blocks else 0
        print(f'{size / 1024 / 1024:.1f}MB  {blocks:,} allocs  avg {avg:.0f}B')
        for frame in stack[:6]:
            print(f'    {frame[:150]}')
        print()


if __name__ == '__main__':
    if len(sys.argv) > 2:
        detail(sys.argv[1], sys.argv[2])
    else:
        main(sys.argv[1] if len(sys.argv) > 1 else 'dhat-heap.json')
