#!/usr/bin/env python3
"""
delay-run.py - Bounded poll loop helper with sleep intervals.

Usage:
  python3 delay-run.py --every <seconds> --for <duration> | --attempts <count>

Options:
  --every     Interval between polls in seconds (minimum 60)
  --for       Max duration as string like "5m", "1h", "300s"
  --attempts  Max number of sleep iterations
"""

import argparse
import sys
import time
import re


def parse_duration(s: str) -> int:
    """Parse duration string like 90s, 5m, 1h into seconds."""
    s = s.strip()
    match = re.fullmatch(r"(\d+)(s|m|h)?", s, re.IGNORECASE)
    if not match:
        raise ValueError(f"Invalid duration: {s!r}")
    value = int(match.group(1))
    unit = (match.group(2) or "s").lower()
    return value * {"s": 1, "m": 60, "h": 3600}[unit]


def main():
    parser = argparse.ArgumentParser(description="Bounded poll loop helper")
    parser.add_argument("--every", required=True, help="Interval in seconds (int or string like 90s, 5m)")
    parser.add_argument("--for", dest="for_duration", default=None, help="Max duration string (e.g. 5m, 300s)")
    parser.add_argument("--attempts", type=int, default=None, help="Max number of sleep iterations")
    args = parser.parse_args()

    # Parse interval
    try:
        interval = parse_duration(args.every)
    except ValueError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        sys.exit(1)

    if interval < 60:
        print(f"ERROR: interval must be at least 60s (got {interval}s)", file=sys.stderr)
        sys.exit(1)

    # Determine max attempts
    if args.for_duration and args.attempts:
        print("ERROR: specify --for or --attempts, not both", file=sys.stderr)
        sys.exit(1)

    if args.for_duration:
        try:
            total_seconds = parse_duration(args.for_duration)
        except ValueError as e:
            print(f"ERROR: {e}", file=sys.stderr)
            sys.exit(1)
        max_attempts = max(1, total_seconds // interval)
    elif args.attempts:
        max_attempts = args.attempts
    else:
        print("ERROR: must specify --for or --attempts", file=sys.stderr)
        sys.exit(1)

    for i in range(1, max_attempts + 1):
        print(f"attempt={i} sleeping={interval}s", flush=True)
        time.sleep(interval)
        print(f"attempt={i} done", flush=True)

    print(f"completed attempts={max_attempts} interval={interval}s", flush=True)


if __name__ == "__main__":
    main()
