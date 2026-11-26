"""Tests of scripts/check-desktop-size.py: python3 -m unittest discover -s scripts/tests"""
import importlib.util
import io
import os
import pathlib
import struct
import tempfile
import unittest
import unittest.mock
from contextlib import redirect_stderr, redirect_stdout

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "check-desktop-size.py"
SPEC = importlib.util.spec_from_file_location("check_desktop_size", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)

EM_AARCH64 = 183


class SizeTest(unittest.TestCase):
    def test_the_mapped_sections_count_and_the_symbols_do_not(self):
        sections = {".text": 600, ".rodata": 200, ".eh_frame": 100, ".symtab": 5000, ".debug_line": 9000}
        with tempfile.TemporaryDirectory() as tmp:
            path = elf_file(tmp, sections)
            code, out, err = run(str(path), "--budget-bytes", "900")
            self.assertEqual(code, 0, err)
            self.assertIn("| .text ", out)
            self.assertNotIn(".symtab", out)
            code, _, err = run(str(path), "--budget-bytes", "899")
        self.assertEqual(code, 1)
        self.assertIn("maps 900 bytes of code and data, over its budget of 899", err)

    def test_close_to_the_budget_only_warns(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = elf_file(tmp, {".text": 960})
            code, out, _ = run(str(path), "--budget-bytes", "1000")
        self.assertEqual(code, 0)
        self.assertIn("::warning::", out)

    def test_another_machine_is_printed_without_a_budget(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = elf_file(tmp, {".text": 5000}, machine=EM_AARCH64)
            code, out, _ = run(str(path), "--budget-bytes", "10")
        self.assertEqual(code, 0)
        self.assertIn("(aarch64)", out)
        self.assertIn("No budget for this machine", out)

    def test_a_file_that_is_no_elf_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "teamcraft-desktop"
            path.write_bytes(b"MZ not an ELF")
            code, _, err = run(str(path))
        self.assertEqual(code, 2)
        self.assertIn("not an ELF file", err)

    def test_a_truncated_file_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = elf_file(tmp, {".text": 10})
            path.write_bytes(path.read_bytes()[:80])
            code, _, err = run(str(path))
        self.assertEqual(code, 2)
        self.assertIn("no readable section headers", err)


def elf_file(tmp, sections, machine=check.EM_X86_64):
    """A 64-bit little-endian ELF file with only a header and section headers of the given sizes."""
    names = b"\0" + b"".join(name.encode() + b"\0" for name in list(sections) + [".shstrtab"])
    entries = [(0, 0)]  # The null section.
    offset = 1
    for name, size in sections.items():
        entries.append((offset, size))
        offset += len(name) + 1
    names_offset = 64
    entries.append((offset, len(names)))
    headers_offset = names_offset + len(names)
    header = bytearray(64)
    header[0:4] = b"\x7fELF"
    header[4] = 2  # 64-bit
    header[5] = 1  # little-endian
    struct.pack_into("<H", header, 18, machine)
    struct.pack_into("<Q", header, 40, headers_offset)
    struct.pack_into("<HHH", header, 58, 64, len(entries), len(entries) - 1)
    section_headers = b""
    for index, (name_offset, size) in enumerate(entries):
        file_offset = names_offset if index == len(entries) - 1 else 0
        section_headers += struct.pack("<IIQQQQIIQQ", name_offset, 0, 0, 0, file_offset, size, 0, 0, 0, 0)
    path = pathlib.Path(tmp) / "teamcraft-desktop"
    path.write_bytes(bytes(header) + names + section_headers)
    return path


def run(*argv):
    out, err = io.StringIO(), io.StringIO()
    with unittest.mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": ""}), redirect_stdout(out), redirect_stderr(err):
        code = check.main(list(argv))
    return code, out.getvalue(), err.getvalue()


if __name__ == "__main__":
    unittest.main()
