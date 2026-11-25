#!/usr/bin/env python3
"""Checks the Linux desktop executable, as the packages ship it (stripped), against its size budget.

The executable is mapped into the app's memory: every page of code the start and a session run is
in the shell's resident set, and the kernel maps code around each page it faults in, so a larger
binary costs memory even where most of it never runs (docs/performance.md, "Desktop on Linux: a
smaller binary and less wasm at the start", #2384). The budget is on the code and read-only data
the loader maps (`.text`, `.rodata` and the unwind tables), read from the ELF section headers, so it
doesn't move with the symbol table or debug info a build may still carry.

Only x86_64 has a budget; other machines are printed for information (no measurement to set one
from yet).

        scripts/check-desktop-size.py target/release/teamcraft-desktop [--budget-bytes N]

With `GITHUB_STEP_SUMMARY` set, the table also goes to the job summary.
"""
import argparse
import os
import pathlib
import struct
import sys

# The mapped sections of the x86_64 release build (the sum of SECTIONS), decimal megabytes:
# 128.45 MB on 2026-11-25 before #2384, __AFTER__ after it (fat LTO, one codegen unit), plus about 5 %.
DEFAULT_BUDGET_BYTES = 130_000_000
# Warn when the total is above this share of the budget.
WARN_SHARE = 0.95
# What the loader maps and the budget counts: code, read-only data and the tables for unwinding.
SECTIONS = (".text", ".rodata", ".data.rel.ro", ".eh_frame", ".eh_frame_hdr", ".gcc_except_table")
ELF_MAGIC = b"\x7fELF"
EM_X86_64 = 62
MACHINES = {EM_X86_64: "x86_64", 183: "aarch64"}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("executable", type=pathlib.Path, help="the stripped desktop executable")
    parser.add_argument("--budget-bytes", type=int, default=DEFAULT_BUDGET_BYTES,
                        help="fail when the mapped sections of an x86_64 build are above this")
    args = parser.parse_args(argv)
    try:
        machine, sections = read_sections(args.executable.read_bytes())
    except (OSError, ValueError) as error:
        print(f"error: {args.executable}: {error}", file=sys.stderr)
        return 2
    total = sum(sections.get(name, 0) for name in SECTIONS)
    report = render(args.executable, machine, sections, total, args.budget_bytes)
    print(report)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as file:
            file.write(report + "\n")
    if machine != EM_X86_64:
        return 0
    if total > args.budget_bytes:
        print(f"error: the desktop executable maps {total:,} bytes of code and data, over its budget of "
              f"{args.budget_bytes:,}", file=sys.stderr)
        return 1
    if total > args.budget_bytes * WARN_SHARE:
        # A workflow annotation where CI runs it; plain text elsewhere.
        print(f"::warning::the desktop executable maps {total:,} bytes, within "
              f"{100 - WARN_SHARE * 100:.0f} % of its budget of {args.budget_bytes:,}")
    return 0


def read_sections(data):
    """(e_machine, {section name: size}) of a 64-bit little-endian ELF file."""
    if data[:4] != ELF_MAGIC:
        raise ValueError("not an ELF file")
    if data[4] != 2 or data[5] != 1:
        raise ValueError("not a 64-bit little-endian ELF file")
    machine = struct.unpack_from("<H", data, 18)[0]
    section_offset = struct.unpack_from("<Q", data, 40)[0]
    entry_size, count, names_index = struct.unpack_from("<HHH", data, 58)
    if count == 0 or names_index >= count or section_offset + count * entry_size > len(data):
        raise ValueError("no readable section headers")
    headers = [struct.unpack_from("<IIQQQQIIQQ", data, section_offset + index * entry_size)
               for index in range(count)]
    names_start = headers[names_index][4]
    sections = {}
    for header in headers:
        name_start = names_start + header[0]
        name_end = data.index(b"\0", name_start)
        sections[data[name_start:name_end].decode("ascii", "replace")] = header[5]
    return machine, sections


def render(path, machine, sections, total, budget_bytes):
    name = MACHINES.get(machine, f"machine {machine}")
    lines = [f"| Desktop executable ({name})  | Size       |",
             "|-------------------------------|------------|"]
    for section in SECTIONS:
        if section in sections:
            lines.append(f"| {section:<29} | {mb(sections[section]):>10} |")
    lines.append(f"| {'mapped total':<29} | {mb(total):>10} |")
    lines.append(f"| {'file':<29} | {mb(path.stat().st_size):>10} |")
    lines.append("")
    if machine == EM_X86_64:
        lines.append(f"Budget {mb(budget_bytes)}, headroom {mb(budget_bytes - total)}.")
    else:
        lines.append("No budget for this machine; printed for information.")
    return "\n".join(lines)


def mb(size):
    return f"{size / 1_000_000:.2f} MB"


if __name__ == "__main__":
    sys.exit(main())
