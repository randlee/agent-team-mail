#!/usr/bin/env python3
"""Minimal tmux nudge hook for ATM post-send rules."""

from __future__ import annotations

import subprocess
import sys
import time


def main() -> int:
    if len(sys.argv) != 2:
        return 2

    recipient = sys.argv[1]
    pane_map = {
        "team-lead": "atm-dev:1.1",
        "arch-ctm": "atm-dev:1.2",
    }
    pane = pane_map.get(recipient)
    if pane is None:
        return 0

    message = "You have unread ATM messages. Run: atm read --team atm-dev"

    subprocess.run(["tmux", "send-keys", "-t", pane, "-l", message], check=True)
    time.sleep(0.5)
    subprocess.run(["tmux", "send-keys", "-t", pane, "Enter"], check=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
