#!/usr/bin/env python3
"""
Parse Criterion benchmark results and generate a nicely formatted summary table.
"""

import json
import sys
from pathlib import Path
from typing import Dict, List, Tuple
from datetime import date
from uuid import uuid4


def parse_criterion_results(criterion_dir: Path) -> Dict:
    """Parse Criterion JSON results."""
    results = {}

    # Find all benchmark results
    for bench_dir in criterion_dir.iterdir():
        if not bench_dir.is_dir():
            continue

        bench_name = bench_dir.name

        # Look for parameter subdirectories (10, 100, 1000)
        for param_dir in bench_dir.iterdir():
            if not param_dir.is_dir():
                continue

            # Try to read estimates.json
            estimates_file = param_dir / "new" / "estimates.json"
            if not estimates_file.exists():
                continue

            try:
                with open(estimates_file) as f:
                    data = json.load(f)

                # Get mean time in nanoseconds
                mean_ns = data["mean"]["point_estimate"]

                if bench_name not in results:
                    results[bench_name] = {}

                results[bench_name][param_dir.name] = mean_ns

            except (json.JSONDecodeError, KeyError):
                continue

    return results


def format_time(ns: float) -> str:
    """Format time in human-readable format with consistent width."""
    if ns < 1000:  # Less than 1 microsecond
        return f"{ns:.0f} ns"
    elif ns < 1_000_000:  # Less than 1 millisecond
        us = ns / 1000
        return f"{us:.2f} µs"
    elif ns < 1_000_000_000:  # Less than 1 second
        ms = ns / 1_000_000
        return f"{ms:.2f} ms"
    else:
        s = ns / 1_000_000_000
        return f"{s:.2f} s"


def format_benchmark_name(name: str) -> str:
    """Format benchmark name for display."""
    # Replace underscores with spaces
    display = name.replace("_", " ")

    # Special case formatting
    replacements = {
        "helix index": "Helix Index",
        "git status": "Git Status",
        "query staged": "Query Staged",
        "index read": "Index Read",
    }

    for old, new in replacements.items():
        if display.startswith(old):
            display = display.replace(old, new, 1)
            break

    return display.title()


def build_aligned_table(results: Dict) -> str:
    """Build results as a nicely aligned markdown table and return it as a string."""

    # Get all unique parameters (10, 100, 1000) sorted
    all_params = sorted(
        set(
            param
            for bench_results in results.values()
            for param in bench_results.keys()
        ),
        key=lambda x: int(x) if x.isdigit() else 0,
    )

    # Prepare all rows
    rows = []
    for bench_name in sorted(results.keys()):
        bench_results = results[bench_name]
        display_name = format_benchmark_name(bench_name)

        row_data = [display_name]
        for param in all_params:
            if param in bench_results:
                time_str = format_time(bench_results[param])
                row_data.append(time_str)
            else:
                row_data.append("N/A")

        rows.append(row_data)

    # Calculate column widths
    headers = ["Benchmark"] + [f"{p} files" for p in all_params]
    col_widths = [len(h) for h in headers]

    for row in rows:
        for i, cell in enumerate(row):
            col_widths[i] = max(col_widths[i], len(cell))

    lines = []

    # Header
    header_line = (
        "| " + " | ".join(h.ljust(w) for h, w in zip(headers, col_widths)) + " |"
    )
    separator = "|" + "|".join("-" * (w + 2) for w in col_widths) + "|"

    lines.append(header_line)
    lines.append(separator)

    # Rows
    for row in rows:
        row_line = (
            "| "
            + " | ".join(
                cell.rjust(w) if i > 0 else cell.ljust(w)
                for i, (cell, w) in enumerate(zip(row, col_widths))
            )
            + " |"
        )
        lines.append(row_line)

    return "\n".join(lines)


def print_aligned_table(results: Dict):
    """Print the aligned table (kept for backwards compatibility)."""
    print(build_aligned_table(results))


def calculate_speedup(git_time: float, helix_time: float) -> str:
    """Calculate speedup ratio."""
    if helix_time == 0:
        return "∞"
    speedup = git_time / helix_time
    if speedup > 1:
        return f"{speedup:.1f}x faster"
    elif speedup < 1:
        return f"{1/speedup:.1f}x slower"
    else:
        return "same"


def build_comparison_table(results: Dict) -> str:
    """Build comparison table between git and helix as markdown and return it."""

    git_baseline = results.get("git_status_baseline", {})
    helix_cached = results.get("helix_index_cached_run", {})
    helix_first = results.get("helix_index_first_run", {})

    if not git_baseline or not helix_cached:
        return ""

    lines = []
    lines.append("## Performance Comparison\n")

    all_params = sorted(git_baseline.keys(), key=lambda x: int(x) if x.isdigit() else 0)

    # Prepare comparison rows
    comparison_rows = []

    for param in all_params:
        if param not in git_baseline:
            continue

        git_time = git_baseline[param]

        # Git baseline
        comparison_rows.append(
            [f"Git status ({param} files)", format_time(git_time), "-", "-"]
        )

        # Helix first run
        if param in helix_first:
            helix_time = helix_first[param]
            speedup = calculate_speedup(git_time, helix_time)
            comparison_rows.append(
                [f"  Helix (first run)", format_time(helix_time), speedup, ""]
            )

        # Helix cached
        if param in helix_cached:
            helix_time = helix_cached[param]
            speedup = calculate_speedup(git_time, helix_time)
            comparison_rows.append(
                [f"  Helix (cached)", format_time(helix_time), speedup, "⚡"]
            )

        comparison_rows.append(["", "", "", ""])  # Blank row

    # Calculate column widths
    headers = ["Operation", "Time", "vs Git", ""]
    col_widths = [len(h) for h in headers]

    for row in comparison_rows:
        for i, cell in enumerate(row):
            col_widths[i] = max(col_widths[i], len(cell))

    header_line = (
        "| " + " | ".join(h.ljust(w) for h, w in zip(headers, col_widths)) + " |"
    )
    separator = "|" + "|".join("-" * (w + 2) for w in col_widths) + "|"

    lines.append(header_line)
    lines.append(separator)

    # Rows
    for row in comparison_rows:
        if all(cell == "" for cell in row):
            continue
        row_line = (
            "| " + " | ".join(cell.ljust(w) for cell, w in zip(row, col_widths)) + " |"
        )
        lines.append(row_line)

    return "\n".join(lines)


def print_comparison_table(results: Dict):
    """Print comparison table (kept for backwards compatibility)."""
    s = build_comparison_table(results)
    if s:
        print(s)


def build_summary_stats(results: Dict) -> str:
    """Build summary statistics as markdown and return it."""

    git_baseline = results.get("git_status_baseline", {})
    helix_cached = results.get("helix_index_cached_run", {})
    query_staged = results.get("query_staged", {})

    if not git_baseline or not helix_cached:
        return ""

    lines = []
    lines.append("## Summary Statistics\n")

    # Calculate average speedup
    speedups = []
    for param in git_baseline:
        if param in helix_cached:
            git_time = git_baseline[param]
            helix_time = helix_cached[param]
            if helix_time > 0:
                speedups.append(git_time / helix_time)

    if speedups:
        avg_speedup = sum(speedups) / len(speedups)
        lines.append(
            f"**Average speedup (cached):** {avg_speedup:.1f}x faster than git"
        )

    # Query performance
    if query_staged:
        avg_query = sum(query_staged.values()) / len(query_staged)
        lines.append(f"**Average query time:** {format_time(avg_query)}")

    # Load time
    helix_open = results.get("helix_index_open", {})
    if helix_open:
        avg_load = sum(helix_open.values()) / len(helix_open)
        lines.append(f"**Average index load time:** {format_time(avg_load)}")

    return "\n".join(lines)


def print_summary_stats(results: Dict):
    """Print summary stats (kept for backwards compatibility)."""
    s = build_summary_stats(results)
    if s:
        print(s)


def main():
    # Find criterion directory
    criterion_dir = Path("target/criterion")

    if not criterion_dir.exists():
        print(
            "Error: No criterion results found. Run 'cargo bench' first.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Parse results
    results = parse_criterion_results(criterion_dir)

    if not results:
        print("Error: No benchmark results found.", file=sys.stderr)
        sys.exit(1)

    # Build sections
    title = "# Helix Benchmark Results\n"
    aligned_section = "## All Operations\n\n" + build_aligned_table(results)
    comparison_section = build_comparison_table(results)
    summary_section = build_summary_stats(results)

    # Print to stdout (keep existing behavior)
    print(title)
    print(aligned_section)
    if comparison_section:
        print()
        print(comparison_section)
    if summary_section:
        print()
        print(summary_section)

    today_str = date.today().isoformat()
    script_dir = Path(__file__).resolve().parent
    archive_dir = script_dir.parent / "benches" / "archive"
    archive_dir.mkdir(parents=True, exist_ok=True)

    # Find existing archives for today
    existing = sorted(archive_dir.glob(f"{today_str}-*.md"))

    if existing:
        # Extract last counter and increment
        last_file = existing[-1]
        last_counter = int(last_file.stem.split("-")[-1])
        next_counter = last_counter + 1
    else:
        next_counter = 1

    archive_path = archive_dir / f"{today_str}-{next_counter:03d}.md"

    full_md = [title, aligned_section]
    if comparison_section:
        full_md.append("\n" + comparison_section)
    if summary_section:
        full_md.append("\n" + summary_section)

    archive_content = "\n\n".join(full_md).rstrip() + "\n"

    with open(archive_path, "w", encoding="utf-8") as f:
        f.write(archive_content)

    print(f"\nWrote archive to {archive_path}")


if __name__ == "__main__":
    main()
