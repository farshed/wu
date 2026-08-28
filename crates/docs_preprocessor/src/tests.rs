use super::*;
use serde_json::json;

#[test]
fn test_find_binding_prefers_exact_match_over_parameterized() {
    let keymap: KeymapFile = serde_json::from_value(json!([
        {
            "bindings": {
                "ctrl-tab": "projects::OpenRecent",
                "ctrl-shift-tab": ["projects::OpenRecent", { "create_new_window": true }]
            }
        }
    ]))
    .unwrap();

    let binding = find_binding_in_keymap(&keymap, "projects::OpenRecent");
    assert_eq!(binding.as_deref(), Some("ctrl-tab"));
}

#[test]
fn test_find_binding_falls_back_to_parameterized_match() {
    let keymap: KeymapFile = serde_json::from_value(json!([
        {
            "bindings": {
                "ctrl-shift-tab": ["projects::OpenRecent", { "create_new_window": true }]
            }
        }
    ]))
    .unwrap();

    let binding = find_binding_in_keymap(&keymap, "projects::OpenRecent");
    assert_eq!(binding.as_deref(), Some("ctrl-shift-tab"));
}

#[test]
fn test_find_binding_prefers_exact_match_regardless_of_order() {
    let keymap: KeymapFile = serde_json::from_value(json!([
        {
            "bindings": {
                "ctrl-shift-tab": ["projects::OpenRecent", { "create_new_window": true }],
                "ctrl-tab": "projects::OpenRecent"
            }
        }
    ]))
    .unwrap();

    let binding = find_binding_in_keymap(&keymap, "projects::OpenRecent");
    assert_eq!(binding.as_deref(), Some("ctrl-tab"));
}

#[test]
fn test_find_binding_later_section_overrides_earlier() {
    let keymap: KeymapFile = serde_json::from_value(json!([
        { "bindings": { "ctrl-a": "some::Action" } },
        { "bindings": { "ctrl-b": "some::Action" } }
    ]))
    .unwrap();

    let binding = find_binding_in_keymap(&keymap, "some::Action");
    assert_eq!(binding.as_deref(), Some("ctrl-b"));
}
