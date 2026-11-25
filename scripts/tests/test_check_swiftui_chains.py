"""Tests for scripts/check-swiftui-chains.py (issue #2392)."""
import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "check-swiftui-chains.py"
spec = importlib.util.spec_from_file_location("check_swiftui_chains", SCRIPT)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


def chain(modifiers, indent="    ", base_indent="    "):
    """A body: a base view and its modifiers, each with a closure over two lines."""
    lines = ["  var body: some View {", base_indent + "screen"]
    for index in range(modifiers):
        lines += [f"{indent}.sheet(isPresented: $shown{index}) {{", f"{indent}  Text(\"x\")", f"{indent}}}"]
    lines.append("  }")
    return "\n".join(lines) + "\n"


class CheckSwiftUiChainsTest(unittest.TestCase):
    def test_a_chain_up_to_the_limit_passes(self):
        self.assertEqual(check.long_chains(chain(3), 3), [])

    def test_a_longer_chain_is_found_at_its_first_modifier(self):
        self.assertEqual(check.long_chains(chain(4), 3), [(3, 4)])

    def test_modifiers_indented_below_their_view_count_too(self):
        self.assertEqual(check.long_chains(chain(4, indent="      "), 3), [(3, 4)])

    def test_trailing_closure_labels_continue_the_chain(self):
        source = (
            "    screen\n"
            "    .alert(\"a\", isPresented: $a) {\n"
            "      Button(\"done\") {}\n"
            "    } message: {\n"
            "      Text(\"m\")\n"
            "    }\n"
            "    // A comment between modifiers.\n"
            "    .padding()\n"
        )
        self.assertEqual(check.long_chains(source, 1), [(2, 2)])

    def test_a_sibling_view_starts_a_new_chain(self):
        source = "    Text(\"a\")\n      .padding()\n      .bold()\n    Text(\"b\")\n      .padding()\n"
        self.assertEqual(check.long_chains(source, 1), [(2, 2)])

    def test_modifiers_inside_a_closure_are_their_own_chain(self):
        source = (
            "    screen\n"
            "    .overlay {\n"
            "      Text(\"a\")\n"
            "        .padding()\n"
            "        .bold()\n"
            "    }\n"
            "    .padding()\n"
        )
        self.assertEqual(check.long_chains(source, 2), [])


if __name__ == "__main__":
    unittest.main()
