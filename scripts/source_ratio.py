from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXCLUDED = {"target", "vendor", "vendored", "generated", ".git"}
RUST = {".rs"}
NATIVE = {".c", ".cc", ".cpp", ".cxx", ".h", ".hpp"}


def source_lines(suffixes: set[str]) -> int:
    total = 0
    for path in ROOT.rglob("*"):
        if not path.is_file() or path.suffix.lower() not in suffixes:
            continue
        if any(part in EXCLUDED for part in path.parts):
            continue
        total += sum(1 for line in path.read_text(encoding="utf-8").splitlines() if line.strip())
    return total


rust = source_lines(RUST)
native = source_lines(NATIVE)
ratio = 100.0 if rust + native == 0 else rust * 100.0 / (rust + native)
print(f"Rust={rust} native={native} Rust ratio={ratio:.2f}%")
if ratio < 90.0:
    raise SystemExit("Rust source ratio is below 90%")
