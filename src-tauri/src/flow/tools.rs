//! The conductor's flow hands — port of `src/main/lib/flow-tools.js`
//! (168 lines): `read_flow`/`draft_flow`. The one invariant that matters:
//! the model can only ever touch `<workspaceRoot>/.tome/flows/<name>.flow.json`,
//! and only with content `flow::model::validate_flow` accepts structurally.
//!
//! Sync on purpose, mirroring the JS original: these run once per tool
//! call and the conductor calls them un-awaited.
//!
//! Works on `serde_json::Value` directly rather than `flow::model::FlowDoc`:
//! `draft_flow` writes back whatever document the model handed in (with
//! `name`/`version`/coordinates possibly patched in place), and a strict
//! typed round-trip would silently drop any field the model set that this
//! crate's `FlowDoc` doesn't itself model. `flow::model::validate_flow`
//! still does the structural check — this module deserializes into a
//! `FlowDoc` ONLY for that check, on a throwaway clone, never for the
//! bytes actually written to disk.

// Every function below is exercised by its own #[cfg(test)] suite, but in
// a plain (non-test) build nothing calls any of it yet: the real caller is
// the conductor's tool-call dispatch (`flow::tools::read_flow_tool`/
// `draft_flow_tool` — this task's brief: "CONDUCTOR (next stage) calls
// flow::tools::read_flow / draft_flow"), a different slice landing after
// this one. Same rationale and shape as `agent_spawn.rs`'s own top-level
// allow.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::confine::confine_real_abs_sync;
use super::model::unsafe_folder_name;

const SUFFIX: &str = ".flow.json";

fn flows_dir(root: &Path) -> PathBuf {
    root.join(".tome").join("flows")
}

enum PickedRoot<'a> {
    Root(&'a str),
    Error(String),
}

/// Which workspace root the flow lives under. An explicit root must be one
/// of the open folders VERBATIM — compared, never resolved or
/// prefix-matched, the same discipline `agent_spawn`'s allowlist uses.
fn pick_root<'a>(roots: &'a [String], wanted: Option<&str>) -> PickedRoot<'a> {
    if roots.is_empty() {
        return PickedRoot::Error("No workspace folder is open yet — open a folder first.".to_string());
    }
    match wanted {
        None => PickedRoot::Root(&roots[0]),
        Some(w) => match roots.iter().find(|r| r.as_str() == w) {
            Some(r) => PickedRoot::Root(r),
            None => PickedRoot::Error(format!("Unknown root. Open workspace folders: {}", roots.join(", "))),
        },
    }
}

/// Reject before resolve — a name carrying a separator or ".." is an attack
/// or a mistake either way. `None` means the name is fine.
fn bad_name(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return Some("Flow name must be a non-empty string.".to_string());
    }
    if unsafe_folder_name(name) {
        return Some(format!("Flow name \"{name}\" can't contain \"/\", \"\\\", or be \"..\"."));
    }
    None
}

/// Lexical `resolve(dir, name + SUFFIX)` confined to `flows_dir(root)` —
/// mirrors `flowPath`'s own `resolve()` + `startsWith` check (a LEXICAL
/// guard; [`confine_real_abs_sync`] re-checks the REAL location at every
/// actual read/write below).
fn flow_path(root: &Path, name: &str) -> Option<PathBuf> {
    let dir = flows_dir(root);
    let abs = dir.join(format!("{name}{SUFFIX}"));
    if abs.starts_with(&dir) && abs != dir {
        Some(abs)
    } else {
        None
    }
}

/// List without a name, raw document text with one — text rather than a
/// parsed object, since the tool protocol is strings anyway and re-encoding
/// would only launder hand-edits.
pub fn read_flow_tool(roots: &[String], root_arg: Option<&str>, name: Option<&str>) -> String {
    let picked_root = match pick_root(roots, root_arg) {
        PickedRoot::Error(e) => return e,
        PickedRoot::Root(r) => r,
    };

    let Some(name) = name else {
        let mut names = Vec::new();
        let search_roots: Vec<&str> = if root_arg.is_some() { vec![picked_root] } else { roots.iter().map(String::as_str).collect() };
        for root in search_roots {
            let Ok(entries) = std::fs::read_dir(flows_dir(Path::new(root))) else {
                continue; // no .tome/flows yet — an empty workspace, not an error
            };
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let file_name = file_name.to_string_lossy();
                let Some(stem) = file_name.strip_suffix(SUFFIX) else { continue };
                names.push(if roots.len() > 1 { format!("{stem} (in {root})") } else { stem.to_string() });
            }
        }
        return if names.is_empty() { "No flows exist yet.".to_string() } else { names.join("\n") };
    };

    if let Some(bad) = bad_name(name) {
        return bad;
    }
    let search_roots: Vec<&str> = if root_arg.is_some() { vec![picked_root] } else { roots.iter().map(String::as_str).collect() };
    for root in search_roots {
        let root_path = Path::new(root);
        if let Some(abs) = flow_path(root_path, name) {
            if abs.exists() {
                if let Some(confined) = confine_real_abs_sync(root_path, &abs, true) {
                    if let Ok(text) = std::fs::read_to_string(&confined) {
                        return text;
                    }
                }
            }
        }
    }
    format!("No flow named \"{name}\". Call read_flow without a name to list them.")
}

pub struct DraftResult {
    pub text: String,
    pub open_path: Option<PathBuf>,
}

/// Nodes the model sent without coordinates get a left-to-right layout by
/// dependency depth. Depth via bounded edge relaxation (`nodes.len()`
/// passes), which simply stops improving on a cycle instead of looping
/// forever.
fn auto_layout(flow: &mut Map<String, Value>) {
    let nodes = flow.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    let has_coords = nodes.iter().all(|n| {
        n.get("x").and_then(Value::as_f64).is_some_and(f64::is_finite)
            && n.get("y").and_then(Value::as_f64).is_some_and(f64::is_finite)
    });
    if has_coords {
        return;
    }
    let ids: Vec<String> = nodes.iter().filter_map(|n| n.get("id").and_then(Value::as_str)).map(str::to_string).collect();
    let edges = flow.get("edges").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut depth: std::collections::HashMap<String, i64> = ids.iter().map(|id| (id.clone(), 0)).collect();
    for _ in 0..nodes.len().max(1) {
        for edge in &edges {
            let (Some(from), Some(to)) = (edge.get("from").and_then(Value::as_str), edge.get("to").and_then(Value::as_str)) else { continue };
            if let (Some(&d_from), true) = (depth.get(from), depth.contains_key(to)) {
                let candidate = d_from + 1;
                if candidate > depth[to] {
                    depth.insert(to.to_string(), candidate);
                }
            }
        }
    }
    let mut rows: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let Some(node_array) = flow.get_mut("nodes").and_then(Value::as_array_mut) else { return };
    for node in node_array.iter_mut() {
        let has_xy = node.get("x").and_then(Value::as_f64).is_some_and(f64::is_finite)
            && node.get("y").and_then(Value::as_f64).is_some_and(f64::is_finite);
        if has_xy {
            continue;
        }
        let id = node.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let d = *depth.get(&id).unwrap_or(&0);
        let row = *rows.get(&d).unwrap_or(&0);
        rows.insert(d, row + 1);
        if let Some(obj) = node.as_object_mut() {
            obj.insert("x".to_string(), json!(40 + d * 300));
            obj.insert("y".to_string(), json!(40 + row * 170));
        }
    }
}

/// Returns `{ text }` — or `{ text, open_path }` when the file is new, so
/// the conductor can ask the renderer to open a pane for it exactly once;
/// every later overwrite reaches the already-open pane through the disk
/// watcher instead.
pub fn draft_flow_tool(roots: &[String], root_arg: Option<&str>, name: Option<&str>, flow: Option<Value>) -> DraftResult {
    let Some(name) = name else {
        return DraftResult { text: "Flow name must be a non-empty string.".to_string(), open_path: None };
    };
    if let Some(bad) = bad_name(name) {
        return DraftResult { text: bad, open_path: None };
    }
    let picked_root = match pick_root(roots, root_arg) {
        PickedRoot::Error(e) => return DraftResult { text: e, open_path: None },
        PickedRoot::Root(r) => r,
    };

    let Some(Value::Object(mut flow_obj)) = flow else {
        return DraftResult {
            text: "draft_flow needs a flow object: {version, name, nodes, edges}.".to_string(),
            open_path: None,
        };
    };
    if !flow_obj.contains_key("nodes") || flow_obj["nodes"].is_null() {
        flow_obj.insert("nodes".to_string(), json!([]));
    }
    if !flow_obj.contains_key("edges") || flow_obj["edges"].is_null() {
        flow_obj.insert("edges".to_string(), json!([]));
    }
    if !flow_obj["nodes"].is_array() || !flow_obj["edges"].is_array() {
        return DraftResult { text: "flow.nodes and flow.edges must be arrays.".to_string(), open_path: None };
    }
    if !flow_obj.contains_key("version") || flow_obj["version"].is_null() {
        flow_obj.insert("version".to_string(), json!(1));
    }
    // The document's own name follows the vetted filename — flow.name
    // becomes Run's handoff folder, so the two must never diverge.
    flow_obj.insert("name".to_string(), json!(name));

    let doc: super::model::FlowDoc = match serde_json::from_value(Value::Object(flow_obj.clone())) {
        Ok(d) => d,
        Err(e) => return DraftResult { text: format!("Refused — structural errors (nothing written):\n- {e}"), open_path: None },
    };
    let validated = super::model::validate_flow(&doc);
    if !validated.errors.is_empty() {
        return DraftResult {
            text: format!("Refused — structural errors (nothing written):\n- {}", validated.errors.join("\n- ")),
            open_path: None,
        };
    }
    auto_layout(&mut flow_obj);

    let root_path = Path::new(picked_root);
    let Some(abs) = flow_path(root_path, name) else {
        return DraftResult { text: format!("Flow name \"{name}\" does not resolve inside .tome/flows."), open_path: None };
    };
    if confine_real_abs_sync(root_path, &abs, false).is_none() {
        return DraftResult { text: format!("Flow name \"{name}\" escapes the workspace folder."), open_path: None };
    }
    let created = !abs.exists();
    if std::fs::create_dir_all(flows_dir(root_path)).is_err() {
        return DraftResult { text: format!("could not create .tome/flows for \"{name}\"."), open_path: None };
    }
    // Same serialization as FlowPanel.save() — the pane's onDiskChanged
    // compares text to spot its own writes.
    let node_count = flow_obj.get("nodes").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    let edge_count = flow_obj.get("edges").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    let text_to_write = serde_json::to_string_pretty(&Value::Object(flow_obj)).unwrap_or_default() + "\n";
    if std::fs::write(&abs, &text_to_write).is_err() {
        return DraftResult { text: format!("could not write \"{name}\"."), open_path: None };
    }

    let mut text = format!("{} \"{name}\" ({node_count} nodes, {edge_count} edges).", if created { "Created" } else { "Updated" });
    if !validated.warnings.is_empty() {
        text.push_str("\nContract warnings to raise with the user:\n- ");
        text.push_str(&validated.warnings.join("\n- "));
    }
    DraftResult { text, open_path: if created { Some(abs) } else { None } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        tempfile::Builder::new().prefix("tome-flow-tools-").tempdir().unwrap().keep()
    }

    fn flows_dir_of(root: &Path) -> PathBuf {
        flows_dir(root)
    }

    fn valid_flow() -> Value {
        json!({
            "version": 1,
            "name": "anything",
            "nodes": [
                {"id":"n1","kind":"claude","name":"Researcher","instructions":"dig","expects":"a topic","produces":"notes","inputs":[],"outputs":[{"name":"notes"}]},
                {"id":"n2","kind":"claude","name":"Writer","instructions":"write","expects":"notes","produces":"a draft","inputs":[{"name":"notes"}],"outputs":[]},
            ],
            "edges": [{"id":"e1","from":"n1","to":"n2","fromOutput":"notes","toInput":"notes"}],
        })
    }

    // ---- draft_flow_tool ----

    #[test]
    fn refuses_everything_until_a_workspace_folder_is_open() {
        let result = draft_flow_tool(&[], None, Some("x"), Some(valid_flow()));
        assert!(result.text.to_lowercase().contains("workspace folder"));
    }

    #[test]
    fn refuses_traversal_shaped_names_without_writing_anything() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        for name in ["../escape", "a/b", "a\\b", "..", "", "   "] {
            let result = draft_flow_tool(&roots, None, Some(name), Some(valid_flow()));
            assert!(result.text.to_lowercase().contains("name"), "name={name} text={}", result.text);
            assert!(result.open_path.is_none());
        }
        assert!(!flows_dir_of(&root).exists());
    }

    #[test]
    fn refuses_an_explicit_root_that_is_not_an_open_folder_verbatim() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        let bad_root = format!("{}/sub", root.display());
        let result = draft_flow_tool(&roots, Some(&bad_root), Some("x"), Some(valid_flow()));
        assert!(result.text.to_lowercase().contains("unknown root"));
    }

    #[test]
    fn refuses_non_document_flow_shapes() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        for flow in [Value::Null, json!("a string"), json!(["array"])] {
            let result = draft_flow_tool(&roots, None, Some("x"), Some(flow));
            assert!(result.text.to_lowercase().contains("flow"));
        }
        let bad_shape = json!({"nodes": "nope"});
        let result = draft_flow_tool(&roots, None, Some("x"), Some(bad_shape));
        assert!(result.text.to_lowercase().contains("array"));
        assert!(!flows_dir_of(&root).exists());
    }

    #[test]
    fn refuses_structural_errors_without_writing() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        let mut flow = valid_flow();
        let first_node = flow["nodes"][0].clone();
        flow["nodes"].as_array_mut().unwrap().push(first_node); // duplicate node id
        let result = draft_flow_tool(&roots, None, Some("dup"), Some(flow));
        assert!(result.text.contains("structural errors"));
        assert!(result.text.contains("duplicate node id"));
        assert!(!flows_dir_of(&root).exists());
    }

    #[test]
    fn writes_a_valid_flow_and_reports_create_then_update() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        let first = draft_flow_tool(&roots, None, Some("pipeline"), Some(valid_flow()));
        assert!(first.text.starts_with("Created \"pipeline\" (2 nodes, 1 edges)"), "{}", first.text);
        let expected_path = flows_dir_of(&root).join("pipeline.flow.json");
        assert_eq!(first.open_path, Some(expected_path.clone()));

        let raw = std::fs::read_to_string(&expected_path).unwrap();
        let doc: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(raw, serde_json::to_string_pretty(&doc).unwrap() + "\n");
        assert_eq!(doc["name"], json!("pipeline"));
        assert_eq!(doc["version"], json!(1));

        let second = draft_flow_tool(&roots, None, Some("pipeline"), Some(valid_flow()));
        assert!(second.text.starts_with("Updated"), "{}", second.text);
        assert!(second.open_path.is_none());
    }

    #[test]
    fn writes_despite_contract_warnings_and_returns_them() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        let mut flow = valid_flow();
        flow["nodes"][0]["kind"] = json!("mystery-cli");
        let result = draft_flow_tool(&roots, None, Some("warned"), Some(flow));
        assert!(result.text.starts_with("Created"));
        assert!(result.text.to_lowercase().contains("warnings to raise with the user"));
        assert!(result.text.contains("unknown kind \"mystery-cli\""));
        assert!(flows_dir_of(&root).join("warned.flow.json").exists());
    }

    #[test]
    fn defaults_version_and_lays_out_coordinate_less_nodes_left_to_right() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        let mut flow = valid_flow();
        flow.as_object_mut().unwrap().remove("version");
        let result = draft_flow_tool(&roots, None, Some("laid-out"), Some(flow));
        assert!(result.text.starts_with("Created"));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(flows_dir_of(&root).join("laid-out.flow.json")).unwrap()).unwrap();
        assert_eq!(doc["version"], json!(1));
        let a = &doc["nodes"][0];
        let b = &doc["nodes"][1];
        assert!(a["x"].is_number() && a["y"].is_number());
        assert!(b["x"].is_number() && b["y"].is_number());
        assert!(b["x"].as_f64().unwrap() > a["x"].as_f64().unwrap());
    }

    #[test]
    fn leaves_hand_placed_coordinates_alone() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        let mut flow = valid_flow();
        flow["nodes"][0]["x"] = json!(7);
        flow["nodes"][0]["y"] = json!(9);
        draft_flow_tool(&roots, None, Some("mixed"), Some(flow));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(flows_dir_of(&root).join("mixed.flow.json")).unwrap()).unwrap();
        assert_eq!(doc["nodes"][0]["x"], json!(7));
        assert_eq!(doc["nodes"][0]["y"], json!(9));
        assert!(doc["nodes"][1]["x"].is_number());
    }

    // ---- read_flow_tool ----

    #[test]
    fn read_refuses_until_a_workspace_folder_is_open() {
        assert!(read_flow_tool(&[], None, None).to_lowercase().contains("workspace folder"));
    }

    #[test]
    fn read_lists_nothing_gracefully_then_lists_what_draft_flow_wrote() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        assert_eq!(read_flow_tool(&roots, None, None), "No flows exist yet.");
        draft_flow_tool(&roots, None, Some("alpha"), Some(valid_flow()));
        draft_flow_tool(&roots, None, Some("beta"), Some(valid_flow()));
        let listed = read_flow_tool(&roots, None, None);
        let mut names: Vec<&str> = listed.split('\n').collect();
        names.sort_unstable();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn read_returns_the_raw_document_text_by_name_and_a_hint_on_a_miss() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        draft_flow_tool(&roots, None, Some("alpha"), Some(valid_flow()));
        let raw = read_flow_tool(&roots, None, Some("alpha"));
        let doc: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(doc["name"], json!("alpha"));
        assert!(read_flow_tool(&roots, None, Some("ghost")).contains("No flow named \"ghost\""));
    }

    #[test]
    fn read_applies_the_same_name_guard_as_draft() {
        assert!(read_flow_tool(&["/tmp".to_string()], None, Some("../etc")).to_lowercase().contains("name"));
    }

    #[test]
    fn read_only_reads_flow_json_entries() {
        let root = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        std::fs::create_dir_all(flows_dir_of(&root)).unwrap();
        std::fs::write(flows_dir_of(&root).join("notes.txt"), "not a flow").unwrap();
        draft_flow_tool(&roots, None, Some("alpha"), Some(valid_flow()));
        assert_eq!(read_flow_tool(&roots, None, None), "alpha");
    }

    // ---- symlink confinement (TOME-008 style) ----

    #[test]
    fn refuses_to_write_a_flow_reached_through_a_symlinked_flows_directory() {
        let root = tmp();
        let outside = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        std::fs::create_dir_all(root.join(".tome")).unwrap();
        std::os::unix::fs::symlink(&outside, flows_dir_of(&root)).unwrap();
        let result = draft_flow_tool(&roots, None, Some("escape"), Some(valid_flow()));
        assert!(result.text.contains("escapes the workspace"));
        assert!(result.open_path.is_none());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn refuses_to_read_a_flow_reached_through_a_symlinked_flows_directory() {
        let root = tmp();
        let outside = tmp();
        let roots = vec![root.to_string_lossy().into_owned()];
        std::fs::write(outside.join("planted.flow.json"), serde_json::to_string(&valid_flow()).unwrap()).unwrap();
        std::fs::create_dir_all(root.join(".tome")).unwrap();
        std::os::unix::fs::symlink(&outside, flows_dir_of(&root)).unwrap();
        assert!(read_flow_tool(&roots, None, Some("planted")).contains("No flow named \"planted\""));
    }
}
