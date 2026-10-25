"""Tests for scripts/check-tauri-args.py (issue #397)."""
import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "check-tauri-args.py"
spec = importlib.util.spec_from_file_location("check_tauri_args", SCRIPT)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

DESKTOP = """
#[tauri::command(rename_all = "snake_case")]
/// Doc comment between attribute and function.
pub async fn core_link_start(host: State<'_, CoreHost>, device_name: String, note: Option<String>) -> Result<(), E> {
  Ok(())
}

#[tauri::command]
pub fn set_zoom(app: AppHandle, factor: f64) {}
"""


def commands():
    return {c.name: c for c in check.parse_commands(DESKTOP, "d.rs")}


def problems(ui_source):
    return check.check_calls(ui_source, "u.rs", commands(), {"core_store_health"})


class CheckTauriArgsTest(unittest.TestCase):
    def test_parameters_skip_injected_ones_and_note_options(self):
        link = commands()["core_link_start"]
        self.assertEqual(link.params, {"device_name": False, "note": True})
        self.assertTrue(link.is_snake_case)

    def test_a_command_without_rename_all_is_found(self):
        self.assertFalse(commands()["set_zoom"].is_snake_case)

    def test_matching_json_keys_pass(self):
        source = 'async fn f() { call("core_link_start", &json!({ "device_name": name })).await }\n'
        self.assertEqual(problems(source), [])

    def test_a_camel_case_key_is_found(self):
        source = 'async fn f() { call("core_link_start", &json!({ "deviceName": name })).await }\n'
        self.assertEqual(
            problems(source),
            [
                "u.rs:1: `core_link_start` has no argument `deviceName` (takes ['device_name', 'note'])",
                "u.rs:1: `core_link_start` is called without its argument `device_name`",
            ],
        )

    def test_a_missing_required_argument_is_found_but_an_optional_one_may_be_left_out(self):
        source = 'async fn f() { call("core_link_start", &json!({})).await }\n'
        self.assertEqual(problems(source), ["u.rs:1: `core_link_start` is called without its argument `device_name`"])

    def test_keys_from_a_variable_built_above_the_call(self):
        source = (
            "async fn f() {\n"
            '  let args = json!({ "device_name": name, "note": note });\n'
            '  call::<()>("core_link_start", &args).await\n}\n'
        )
        self.assertEqual(problems(source), [])

    def test_reflect_set_keys_are_read(self):
        source = (
            "async fn f() {\n  let args = js_sys::Object::new();\n"
            '  js_sys::Reflect::set(&args, &"deviceName".into(), &x).ok();\n'
            '  invoke("core_link_start", args.into()).await\n}\n'
        )
        self.assertEqual(len(problems(source)), 2)

    def test_keys_of_an_earlier_function_do_not_leak(self):
        source = (
            'fn a() { let args = json!({ "factor": 1.0 }); }\n'
            'async fn b() { call("core_link_start", &json!({ "device_name": n })).await }\n'
        )
        self.assertEqual(problems(source), [])

    def test_unknown_commands_are_found_unless_the_browser_answers_them(self):
        self.assertEqual(
            problems('fn f() { call("nope", &json!({})) }\n'),
            ["u.rs:1: `nope` is not a #[tauri::command] of the desktop app"],
        )
        self.assertEqual(problems('fn f() { call("core_store_health", &json!({})) }\n'), [])

    def test_no_arguments_found_means_only_the_name_is_checked(self):
        self.assertEqual(problems('fn f() { call("core_link_start", &build()).await }\n'), [])


if __name__ == "__main__":
    unittest.main()
