//! SCIM requests from identity providers (docs/provisioning.md §7): filters of list queries, PATCH
//! paths and operations, and user and group resources. Nothing may panic or loop; what a filter
//! parser accepts is evaluated against the resource of the same input, and a patched user that
//! reads back reads back the same way again.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_proto::scim::{
  Filter, GroupAttributes, PatchPath, PatchRequest, UserAttributes, apply_user_patch, group_changes, normalize_keys,
};

fuzz_target!(|data: &[u8]| {
  if let Ok(text) = std::str::from_utf8(data) {
    if let Ok(filter) = Filter::parse(text) {
      let _ = filter.equality_on("username");
      let resource = serde_json::json!({"username": text, "emails": [{"value": text, "type": "work"}]});
      let _ = filter.matches(&resource);
    }
    let _ = PatchPath::parse(text);
  }
  let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) else {
    return;
  };
  let Ok(value) = normalize_keys(value) else {
    return;
  };
  let _ = GroupAttributes::from_resource(&value);
  if let Ok(request) = serde_json::from_value::<PatchRequest>(value.clone())
    && let Ok(request) = request.checked()
  {
    let _ = group_changes(&request.operations);
    let base = serde_json::json!({"username": "a", "active": true, "name": {}});
    let mut patched = base.clone();
    if apply_user_patch(&mut patched, &request.operations).is_ok()
      && let Ok(user) = UserAttributes::from_resource(&patched)
    {
      assert_eq!(UserAttributes::from_resource(&user.to_patchable()), Ok(user));
    }
  }
  if let Ok(user) = UserAttributes::from_resource(&value) {
    assert_eq!(UserAttributes::from_resource(&user.to_patchable()), Ok(user));
  }
});
