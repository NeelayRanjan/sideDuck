#!/usr/bin/env python3
"""Assert an independent 80% production-source line floor for each implemented core.

LLVM's LCOV line counts are authoritative. Test source is excluded: tests.rs and
the trailing, rustfmt-formatted #[cfg(test)] mod tests in each source file. Any
code following that module is rejected so this filter cannot hide new production
code. No coverage claim is made for native glue, UI, or milestone placeholders.
"""

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES = ("sideduck-tile", "sideduck-brush", "sideduck-input")
FLOOR = 80


def production_end(path):
    """Return the first test-only line; require the test module to be trailing."""
    source = path.read_text().splitlines()
    markers = [i for i, line in enumerate(source) if line == "#[cfg(test)]"]
    if not markers:
        return len(source) + 1
    if len(markers) != 1:
        raise ValueError(f"Review coverage exclusion for multiple test sections: {path}")
    start = markers[0]
    declaration = source[start + 1]
    if declaration == "mod tests;":
        end = start + 1
    elif declaration == "mod tests {":
        # rustfmt indents nested braces; only a column-zero brace ends this module.
        end = next(i for i in range(start + 2, len(source)) if source[i] == "}")
    else:
        raise ValueError(f"Review coverage exclusion for unfamiliar test module: {path}")
    if any(line.strip() for line in source[end + 1 :]):
        raise ValueError(f"Production code follows test module; update coverage filter: {path}")
    return start + 1


def read_crate(crate):
    crate_dir = ROOT / "crates" / crate / "src"
    lines = {}
    current = None
    cutoff = 0
    for record in (ROOT / "target" / "coverage" / f"{crate}.lcov").read_text().splitlines():
        if record.startswith("SF:"):
            current = Path(record[3:]).resolve()
            if not current.is_relative_to(crate_dir) or current.name == "tests.rs":
                current = None
                continue
            cutoff = production_end(current)
        elif record.startswith("DA:") and current is not None:
            number, count, *_ = record[3:].split(",")
            number, count = int(number), int(count)
            if number < cutoff:
                key = (str(current.relative_to(ROOT)), number)
                lines[key] = max(lines.get(key, 0), count)
        elif record == "end_of_record":
            current = None
    if not lines:
        raise ValueError(f"No production-source coverage records for {crate}")
    covered = sum(count > 0 for count in lines.values())
    percent = 100 * covered / len(lines)
    return {
        "crate": crate,
        "covered_lines": covered,
        "instrumented_lines": len(lines),
        "line_percent": percent,
        "floor_percent": FLOOR,
        "passed": covered * 100 >= len(lines) * FLOOR,
        "uncovered": [f"{path}:{line}" for (path, line), count in lines.items() if count == 0],
    }


def main():
    results = [read_crate(crate) for crate in CRATES]
    report = {
        "tool": "cargo-llvm-cov 0.6.21 / Rust 1.90.0 llvm-tools",
        "metric": "LLVM instrumented production-source lines, per crate",
        "excluded": ["test modules", "test source files", "platform glue", "UI", "unimplemented milestone crates"],
        "crates": results,
    }
    destination = ROOT / "target" / "coverage" / "summary.json"
    destination.write_text(json.dumps(report, indent=2) + "\n")
    for result in results:
        print(
            f"{result['crate']}: {result['line_percent']:.2f}% "
            f"({result['covered_lines']}/{result['instrumented_lines']} production lines); "
            f"floor {FLOOR}%: {'PASS' if result['passed'] else 'FAIL'}"
        )
    print(f"Coverage detail: {destination}")
    if not all(result["passed"] for result in results):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
