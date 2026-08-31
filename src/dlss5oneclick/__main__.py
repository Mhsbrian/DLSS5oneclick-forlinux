"""`python -m dlss5oneclick [GAME.exe]` — GUI by default, CLI with an argument."""
from __future__ import annotations

import sys
from pathlib import Path


def _cli(argv: list[str]) -> int:
    from dlss5oneclick import installer

    exe = Path(argv[0])
    if len(argv) > 1 and argv[1] == "--remove":
        for f in installer.uninstall(exe):
            print("removed", f)
        return 0

    def progress(pct: int, msg: str) -> None:
        print(f"\r{pct:3d}% {msg:<70}", end="", flush=True)

    def step(i: int, name: str, state: str, detail: str) -> None:
        if state == "start":
            print(f"\n[{i + 1}/{len(installer.STEPS)}] {name}")
        elif state == "done":
            print(f"\n      ok: {detail}")
        else:
            print(f"\n      FAILED: {detail}")

    try:
        installer.run_all(exe, progress, step)
    except installer.InstallError as e:
        print(f"\nerror: {e}", file=sys.stderr)
        return 1
    print("\nDone. In game: Home -> DLSS 5 Neural Rendering panel -> enable.")
    return 0


def main() -> int:
    args = sys.argv[1:]
    if args and not args[0].startswith("-"):
        return _cli(args)
    from dlss5oneclick.gui import main as gui_main

    return gui_main()


if __name__ == "__main__":
    raise SystemExit(main())
