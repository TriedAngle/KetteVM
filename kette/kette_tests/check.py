#!/usr/bin/env python3
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def run(root: Path, paths: list[Path]) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["cargo", "run", "-q", "-p", "vm", "--", *(str(path) for path in paths)],
        capture_output=True,
        text=True,
        cwd=root,
    )


def check_asserts(root: Path, f: Path, runner: Path) -> tuple[bool, str]:
    source = f.read_text() + "\nTest.summary()\n"
    fd, tmp = tempfile.mkstemp(suffix=".ktt", text=True)
    try:
        with os.fdopen(fd, "w") as h:
            h.write(source)
        proc = run(root, [runner, Path(tmp)])
    finally:
        os.unlink(tmp)
    if proc.returncode != 0:
        detail = proc.stderr.strip().splitlines()[-1] if proc.stderr.strip() else ""
        return False, f"exited {proc.returncode}: {detail}"
    lines = proc.stdout.splitlines()
    if "failed" in lines:
        where = lines[lines.index("failed") + 1] if len(lines) > 1 else "?"
        total = lines[-1] if lines else "?"
        return False, f"{where} of {total} asserts failed:\n" + "\n".join(
            "      " + l for l in lines[: lines.index("failed")]
        )
    if "passed" not in lines:
        return False, f"no summary in output: {lines!r}"
    count = lines[-1] if lines else "0"
    return True, f"{count} asserts"


def main() -> None:
    here = Path(__file__).resolve().parent
    root = here.parent
    files = [Path(a) for a in sys.argv[1:]] or sorted(here.glob("*.ktt"))
    runner = here / "test_runner.ktt"

    checked = 0
    failures = 0
    for f in files:
        if f.name == "test_runner.ktt":
            continue
        checked += 1
        ok, detail = check_asserts(root, f, runner)
        if ok:
            print(f"ok    {f.name} {detail}")
        else:
            print(f"FAIL  {f.name}")
            print("      " + detail)
            failures += 1

    if failures:
        print(f"{failures} of {checked} failed")
        sys.exit(1)
    print(f"{checked} checked, all ok")


if __name__ == "__main__":
    main()
