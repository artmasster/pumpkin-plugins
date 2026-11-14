#!/usr/bin/env python3
"""Type commands into a Pumpkin console that is a TTY, the way Wings runs it on a node.

With a TTY Pumpkin reads the console through rustyline, whose hinter asks the command tree for
suggestions on every key. A piped stdin (the older lab runs) skips all of that, which is how
Basics 0.2.0 (then named Nexus) shipped a server crash: a WASM command with its own suggestions panics Pumpkin 0.2.2
there ("there is no reactor running").

Usage: console-smoke.py <server-dir> <command> [<command> ...]
Env:   SMOKE_NETWORK (docker network, default none), SMOKE_TZ (default Asia/Bangkok),
       SMOKE_LIMIT (seconds before the run is cut off, default 150)
Prints the console without colour codes. Exit 0 = no panic, no unknown command, and the server
stopped on our `stop`; 1 otherwise.
"""
import os
import pty
import re
import select
import sys
import time

IMAGE = "ghcr.io/parkervcp/yolks:debian"
# seconds in total; `timeout --foreground` inside the container ends a stuck server
LIMIT = int(os.environ.get("SMOKE_LIMIT", "150"))


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    srv, cmds = os.path.abspath(sys.argv[1]), sys.argv[2:]
    argv = [
        "docker", "run", "--rm", "-it", "--network", os.environ.get("SMOKE_NETWORK", "none"),
        "--cpus", "2", "--memory", "2g", "-e", "TZ=" + os.environ.get("SMOKE_TZ", "Asia/Bangkok"),
        "-v", f"{srv}:/home/container",
        "-e", f"STARTUP=timeout --foreground -s INT {LIMIT - 20} ./pumpkin", IMAGE,
    ]
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(argv[0], argv)
    buf = b""
    started = time.time()
    queue = list(cmds) + ["stop"]
    next_at = None
    sent_stop = False
    alive = True
    while alive and time.time() - started < LIMIT:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if ready:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                chunk = b""
            if not chunk:
                alive = False
                break
            buf += chunk
        if next_at is None and b"Server is now running" in buf:
            next_at = time.time() + 2.0
        if next_at is not None and queue and time.time() >= next_at:
            if b"panicked" in buf:
                queue.clear()
                continue
            cmd = queue.pop(0)
            sent_stop = cmd == "stop"
            # One key at a time, like a person: the hinter runs after every key
            for ch in cmd:
                os.write(fd, ch.encode())
                time.sleep(0.03)
            os.write(fd, b"\r")
            next_at = time.time() + 2.0
    try:
        os.waitpid(pid, os.WNOHANG if alive else 0)
    except ChildProcessError:
        pass
    text = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", buf.decode("utf-8", "replace")).replace("\r", "")
    noise = re.compile(
        r"Saving level|Joining \d+ threads|Flushing|schedule: waiting|Saving chunks|Report issues"
        r"|for community support|Donating|embedded|Loading minecraft|heavy development")
    print("\n".join(line[:260] for line in text.split("\n") if line.strip() and not noise.search(line)))
    panicked = "panicked" in text
    unknown = "Unknown or incomplete command" in text
    print(f"--- panic={panicked} unknown_command={unknown} stop_sent={sent_stop} exited={not alive} "
          f"secs={time.time() - started:.0f}")
    return 0 if sent_stop and not alive and not panicked and not unknown else 1


if __name__ == "__main__":
    sys.exit(main())
