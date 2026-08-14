//! Minimal Rust reading of the flow document shape `src/shared/flow-model.js`
//! defines, scoped to exactly what `flow::runner` and `flow::tools` touch:
//! `validateFlow`, `composeBootstrapPrompt`, `flowRoot`, `unsafeFolderName`,
//! `safeSegment`. `flow-model.js` itself STAYS renderer JS (the canvas/editor
//! keep using it — see the rewrite plan's "src/shared/** stays JS-only"
//! decision); its own vitest suite (`test/flow.test.js`) is not this slice's
//! file to port. This is a from-scratch port of the subset of its logic a
//! headless runner and the conductor's read/draft tools need — no
//! `addNode`/`addEdge`/`edgeError`/`removeNode`/`createFlow` (canvas-only
//! mutations neither caller performs).
//!
//! `flow::run_plan` owns the DAG scheduling half (`layers`/`runPlan`, the
//! shared/flow-run-plan.js port) — see that module's doc comment for why
//! `flow-model.js`'s own `topoSort` has no separate port here: `run_plan`'s
//! `order` is provably the same sequence.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::agent_spawn::{AGENT_MODELS, AGENTS};

// ---- document shape (deserialized straight from a `<name>.flow.json`) ----

#[derive(Debug, Clone, Deserialize)]
pub struct FlowDoc {
    #[serde(default)]
    pub version: Value,
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<FlowNode>,
    #[serde(default)]
    pub edges: Vec<FlowEdge>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FlowNode {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub expects: Option<String>,
    #[serde(default)]
    pub produces: Option<String>,
    #[serde(default)]
    pub inputs: Vec<FlowPort>,
    #[serde(default)]
    pub outputs: Vec<FlowPort>,
}

impl FlowNode {
    /// `node.name || node.id` — JS's `||` treats an empty string as absent
    /// too, so this falls back on both "missing" and "".
    pub fn display_name(&self) -> &str {
        match self.name.as_deref() {
            Some(n) if !n.is_empty() => n,
            _ => &self.id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FlowPort {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FlowEdge {
    #[serde(default)]
    pub id: Option<String>,
    pub from: String,
    pub to: String,
    #[serde(default, rename = "fromOutput")]
    pub from_output: Option<String>,
    #[serde(default, rename = "toInput")]
    pub to_input: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

// ---- unsafeFolderName / safeSegment ----

/// A flow's `name` becomes a literal filesystem path segment
/// (`.tome/flows/<name>/`) via straight string interpolation — no path
/// separator or bare ".." may reach that mkdir.
pub fn unsafe_folder_name(name: &str) -> bool {
    name == ".." || name.contains('\\') || name.contains('/')
}

/// Port of `safeSegment` — string-shaped input only. Rust's `&str` already
/// makes the JS suite's non-string cases (`undefined`/`null`/a bare `42`)
/// unrepresentable at the type level; same simplification `agent_spawn.rs`'s
/// own doc comment documents for `model`/`brief` there. [`safe_segment_opt`]
/// below is the `Option<&str>` convenience an absent/non-string JS field
/// (`input?.name`, an edge's optional `id`) reduces to: `None` fails exactly
/// like `typeof s !== 'string'` did.
pub fn safe_segment(s: &str) -> bool {
    if s.is_empty() || s == "." || s == ".." {
        return false;
    }
    if s.starts_with('-') {
        return false;
    }
    !s.chars().any(|c| matches!(c, '\\' | '/' | ':') || c.is_control())
}

pub fn safe_segment_opt(s: Option<&str>) -> bool {
    s.is_some_and(safe_segment)
}

fn unsafe_segment_error(what: &str, value: Option<&str>) -> String {
    format!(
        r#"{what} "{}" can't be used in a handoff path (no "/", "\", ":", control characters, or a leading "-")"#,
        value.unwrap_or("undefined")
    )
}

// ---- validateFlow ----

pub struct ValidateResult {
    pub errors: Vec<String>,
    // Read by `flow::tools::draft_flow_tool` (still uncalled in a plain
    // non-test build — see that module's top-level `allow`) and by this
    // module's own tests.
    #[allow(dead_code)]
    pub warnings: Vec<String>,
}

/// Errors mean the *graph* is broken (a dangling reference, an unsafe name —
/// `startRun`/`draftFlowTool` both refuse on any error). Warnings mean only
/// the declared *contract* is off (an unrecognized kind, a stale port name)
/// and never block anything — a hand-edited `flow.json` must still open and
/// run.
pub fn validate_flow(flow: &FlowDoc) -> ValidateResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if flow.version != Value::from(1) {
        warnings.push(format!("unknown flow version \"{}\" (expected 1)", flow.version));
    }

    if unsafe_folder_name(&flow.name) {
        errors.push(format!(
            "flow name \"{}\" can't be used as a folder name (no \"/\", \"\\\", or \"..\")",
            flow.name
        ));
    }

    let mut seen_node_ids: HashSet<&str> = HashSet::new();
    let mut node_by_id: HashMap<&str, &FlowNode> = HashMap::new();
    for node in &flow.nodes {
        if !seen_node_ids.insert(&node.id) {
            errors.push(format!("duplicate node id \"{}\"", node.id));
        }
        node_by_id.insert(&node.id, node);

        if !safe_segment(&node.id) {
            errors.push(unsafe_segment_error("node id", Some(&node.id)));
        }
        for input in &node.inputs {
            let name = input.name.as_deref();
            if !safe_segment_opt(name) {
                errors.push(unsafe_segment_error(&format!("node \"{}\" input name", node.id), name));
            }
        }
        for output in &node.outputs {
            let name = output.name.as_deref();
            if !safe_segment_opt(name) {
                errors.push(unsafe_segment_error(&format!("node \"{}\" output name", node.id), name));
            }
        }

        if node.kind != "terminal" && !AGENTS.contains(&node.kind.as_str()) {
            warnings.push(format!("node \"{}\" has unknown kind \"{}\"", node.id, node.kind));
        }

        if let Some(model) = node.model.as_deref().filter(|m| !m.is_empty()) {
            let allowed = AGENT_MODELS.iter().find(|(k, _)| *k == node.kind).map(|(_, m)| *m).unwrap_or(&[]);
            if !allowed.contains(&model) {
                warnings.push(format!(
                    "node \"{}\" has unknown model \"{}\" for kind \"{}\"",
                    node.id, model, node.kind
                ));
            }
        }
    }

    let mut seen_edge_ids: HashSet<&str> = HashSet::new();
    for edge in &flow.edges {
        if let Some(id) = edge.id.as_deref() {
            if !seen_edge_ids.insert(id) {
                errors.push(format!("duplicate edge id \"{id}\""));
            }
        }
        let edge_label = edge.id.as_deref().unwrap_or("undefined");
        let from_node = node_by_id.get(edge.from.as_str());
        let to_node = node_by_id.get(edge.to.as_str());
        if from_node.is_none() {
            errors.push(format!("edge \"{edge_label}\" references a missing node \"{}\"", edge.from));
        }
        if to_node.is_none() {
            errors.push(format!("edge \"{edge_label}\" references a missing node \"{}\"", edge.to));
        }

        for (field, value) in [
            ("from", Some(edge.from.as_str())),
            ("to", Some(edge.to.as_str())),
            ("fromOutput", edge.from_output.as_deref()),
            ("toInput", edge.to_input.as_deref()),
        ] {
            if !safe_segment_opt(value) {
                errors.push(unsafe_segment_error(&format!("edge \"{edge_label}\" {field}"), value));
            }
        }

        if let Some(from_node) = from_node {
            if !from_node.outputs.iter().any(|o| o.name.as_deref() == edge.from_output.as_deref()) {
                warnings.push(format!(
                    "edge \"{edge_label}\": \"{}\" is not an output of node \"{}\"",
                    edge.from_output.as_deref().unwrap_or("undefined"),
                    edge.from
                ));
            }
        }
        if let Some(to_node) = to_node {
            if !to_node.inputs.iter().any(|i| i.name.as_deref() == edge.to_input.as_deref()) {
                warnings.push(format!(
                    "edge \"{edge_label}\": \"{}\" is not an input of node \"{}\"",
                    edge.to_input.as_deref().unwrap_or("undefined"),
                    edge.to
                ));
            }
        }
    }

    ValidateResult { errors, warnings }
}

// ---- flowRoot ----

/// Where Run spawns its nodes. `composeBootstrapPrompt`'s handoff paths
/// (`.tome/flows/<name>/<node>-<output>.md`) are relative to whatever folder
/// contains this flow's own `.tome` — not the `flow.json`'s own folder, two
/// levels deeper.
pub fn flow_root(path: &str) -> String {
    let marker = "/.tome/";
    if let Some(i) = path.rfind(marker) {
        path[..i].to_string()
    } else if let Some(slash) = path.rfind('/') {
        path[..slash].to_string()
    } else {
        ".".to_string()
    }
}

// ---- composeBootstrapPrompt ----

fn handoff_path(flow_name: &str, node_id: &str, output_name: &str) -> String {
    format!(".tome/flows/{flow_name}/{node_id}-{output_name}.md")
}

/// Builds the text a headless node runs `-p` with — byte for byte what the
/// canvas types into a terminal-mode Run, per this module's own contract
/// (`flow-model.js`'s header: "the runs pane and this file share one
/// definition of the brief").
pub fn compose_bootstrap_prompt(flow: &FlowDoc, node: &FlowNode) -> String {
    let node_by_id: HashMap<&str, &FlowNode> = flow.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let incoming: Vec<&FlowEdge> = flow.edges.iter().filter(|e| e.to == node.id).collect();

    let mut lines = Vec::new();
    lines.push(format!("You are \"{}\" in a Tome flow \"{}\".", node.display_name(), flow.name));
    lines.push(String::new());
    lines.push(format!(
        "Instructions: {}",
        node.instructions.as_deref().filter(|s| !s.is_empty()).unwrap_or("(none given)")
    ));
    lines.push(String::new());
    lines.push("You receive:".to_string());
    lines.push(node.expects.as_deref().filter(|s| !s.is_empty()).unwrap_or("(nothing declared)").to_string());
    for edge in &incoming {
        let upstream_name = node_by_id.get(edge.from.as_str()).map(|n| n.display_name()).unwrap_or(edge.from.as_str());
        let path = handoff_path(&flow.name, &edge.from, edge.from_output.as_deref().unwrap_or(""));
        let described = edge.label.as_deref().filter(|s| !s.is_empty()).map(|l| format!(": {l}")).unwrap_or_default();
        lines.push(format!(
            "- \"{}\" from {upstream_name}{described} (read from {path})",
            edge.from_output.as_deref().unwrap_or("")
        ));
    }
    lines.push(String::new());
    lines.push("You must produce:".to_string());
    lines.push(node.produces.as_deref().filter(|s| !s.is_empty()).unwrap_or("(nothing declared)").to_string());
    for output in &node.outputs {
        lines.push(format!("- {}", output.name.as_deref().unwrap_or("")));
    }
    lines.push(String::new());
    if node.outputs.is_empty() {
        lines.push(format!(
            "Hand off by writing each output to .tome/flows/{}/{}-<output name>.md, then tell the user when you're done.",
            flow.name, node.id
        ));
    } else {
        for output in &node.outputs {
            let name = output.name.as_deref().unwrap_or("");
            let path = handoff_path(&flow.name, &node.id, name);
            lines.push(format!("Hand off \"{name}\" by writing it to {path}, then tell the user when you're done."));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: &str) -> FlowNode {
        FlowNode {
            id: id.to_string(),
            name: None,
            kind: kind.to_string(),
            model: None,
            instructions: None,
            expects: None,
            produces: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    fn doc(name: &str, nodes: Vec<FlowNode>, edges: Vec<FlowEdge>) -> FlowDoc {
        FlowDoc { version: Value::from(1), name: name.to_string(), nodes, edges }
    }

    // ---- safeSegment (test/flow-confine.test.js's own suite; ported here
    // since it's this module's own function — flow::confine's tests import
    // it the same way test/flow-confine.test.js imports it from
    // shared/flow-model.js) ----

    #[test]
    fn safe_segment_accepts_ordinary_identifiers() {
        for s in ["n1", "stale-list", "egress-report", "n-1"] {
            assert!(safe_segment(s), "{s} should be accepted");
        }
    }

    #[test]
    fn safe_segment_rejects_empty_dot_and_dotdot() {
        assert!(!safe_segment(""));
        assert!(!safe_segment("."));
        assert!(!safe_segment(".."));
    }

    #[test]
    fn safe_segment_rejects_a_separator_anywhere_not_just_as_the_whole_value() {
        assert!(!safe_segment("a/b"));
        assert!(!safe_segment("a\\b"));
        assert!(!safe_segment("../../../escaped"));
    }

    #[test]
    fn safe_segment_rejects_colon_control_chars_and_leading_dash() {
        assert!(!safe_segment("a:b"));
        assert!(!safe_segment("a\nb"));
        assert!(!safe_segment("a\0b"));
        assert!(!safe_segment("a\x7fb"));
        assert!(!safe_segment("-rf"));
    }

    #[test]
    fn safe_segment_opt_treats_a_missing_field_like_a_non_string_js_value() {
        assert!(!safe_segment_opt(None));
        assert!(safe_segment_opt(Some("ok")));
    }

    // ---- unsafeFolderName ----

    #[test]
    fn unsafe_folder_name_flags_traversal_and_separators() {
        assert!(unsafe_folder_name(".."));
        assert!(unsafe_folder_name("a/b"));
        assert!(unsafe_folder_name("a\\b"));
        assert!(!unsafe_folder_name("my-flow"));
    }

    // ---- flowRoot ----

    #[test]
    fn flow_root_walks_back_to_the_nearest_dot_tome() {
        assert_eq!(flow_root("/work/proj/.tome/flows/pipeline.flow.json"), "/work/proj");
    }

    #[test]
    fn flow_root_prefers_the_closest_dot_tome_not_the_outermost() {
        assert_eq!(
            flow_root("/work/proj/.tome/flows/nested/.tome/flows/inner.flow.json"),
            "/work/proj/.tome/flows/nested"
        );
    }

    #[test]
    fn flow_root_falls_back_to_the_containing_directory_with_no_dot_tome() {
        assert_eq!(flow_root("/tmp/loose/flow.json"), "/tmp/loose");
        assert_eq!(flow_root("flow.json"), ".");
    }

    // ---- validateFlow — errors ----

    #[test]
    fn validate_flow_errors_on_an_unsafe_flow_name() {
        let f = doc("../escape", vec![node("n1", "claude")], vec![]);
        let result = validate_flow(&f);
        assert!(result.errors.iter().any(|e| e.contains("can't be used as a folder name")));
    }

    #[test]
    fn validate_flow_errors_on_duplicate_node_ids() {
        let f = doc("x", vec![node("n1", "claude"), node("n1", "claude")], vec![]);
        let result = validate_flow(&f);
        assert!(result.errors.iter().any(|e| e.contains("duplicate node id")));
    }

    #[test]
    fn validate_flow_errors_on_a_dangling_edge() {
        let edge = FlowEdge {
            id: Some("e1".to_string()),
            from: "n1".to_string(),
            to: "ghost".to_string(),
            from_output: Some("out".to_string()),
            to_input: Some("in".to_string()),
            label: None,
        };
        let f = doc("x", vec![node("n1", "claude")], vec![edge]);
        let result = validate_flow(&f);
        assert!(result.errors.iter().any(|e| e.contains("missing node")));
    }

    #[test]
    fn validate_flow_errors_on_a_traversal_shaped_node_id() {
        let f = doc("x", vec![node("../../../escaped", "claude")], vec![]);
        let result = validate_flow(&f);
        assert!(result.errors.iter().any(|e| e.contains("can't be used in a handoff path")));
    }

    // ---- validateFlow — warnings only, never block ----

    #[test]
    fn validate_flow_warns_but_does_not_error_on_an_unknown_kind() {
        let f = doc("x", vec![node("n1", "mystery-cli")], vec![]);
        let result = validate_flow(&f);
        assert!(result.errors.is_empty());
        assert!(result.warnings.iter().any(|w| w.contains("unknown kind \"mystery-cli\"")));
    }

    #[test]
    fn validate_flow_never_warns_on_terminal_kind() {
        let f = doc("x", vec![node("n1", "terminal")], vec![]);
        let result = validate_flow(&f);
        assert!(result.warnings.is_empty());
    }

    // ---- composeBootstrapPrompt ----

    #[test]
    fn compose_bootstrap_prompt_includes_the_identity_line_and_handoff_path() {
        let mut n1 = node("n1", "claude");
        n1.outputs = vec![FlowPort { name: Some("out".to_string()) }];
        let f = doc("shape", vec![n1.clone()], vec![]);
        let brief = compose_bootstrap_prompt(&f, &n1);
        assert!(brief.contains("You are \"n1\" in a Tome flow \"shape\"."));
        assert!(brief.contains(".tome/flows/shape/n1-out.md"));
    }

    #[test]
    fn compose_bootstrap_prompt_lists_incoming_handoffs_by_upstream_name() {
        let mut upstream = node("n1", "claude");
        upstream.name = Some("Researcher".to_string());
        upstream.outputs = vec![FlowPort { name: Some("notes".to_string()) }];
        let mut downstream = node("n2", "claude");
        downstream.inputs = vec![FlowPort { name: Some("notes".to_string()) }];
        let edge = FlowEdge {
            id: Some("e1".to_string()),
            from: "n1".to_string(),
            to: "n2".to_string(),
            from_output: Some("notes".to_string()),
            to_input: Some("notes".to_string()),
            label: None,
        };
        let f = doc("pipeline", vec![upstream, downstream.clone()], vec![edge]);
        let brief = compose_bootstrap_prompt(&f, &downstream);
        assert!(brief.contains("from Researcher"));
        assert!(brief.contains(".tome/flows/pipeline/n1-notes.md"));
    }

    #[test]
    fn compose_bootstrap_prompt_defaults_declare_nothing_when_absent() {
        let n1 = node("n1", "claude");
        let f = doc("x", vec![n1.clone()], vec![]);
        let brief = compose_bootstrap_prompt(&f, &n1);
        assert!(brief.contains("(none given)"));
        assert!(brief.contains("(nothing declared)"));
        assert!(brief.contains("Hand off by writing each output to .tome/flows/x/n1-<output name>.md"));
    }
}
