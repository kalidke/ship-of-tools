use serde_json::json;
use sot_protocol::Kind;

use crate::server::reply::HandlerOutput;
use crate::session::Session;

use super::Workspaces;

fn assert_unknown_workspace(out: HandlerOutput, op: &str, hint: &str) {
    assert_eq!(out.len(), 1, "{op}: one frame");
    let (frame, blob) = &out[0];
    assert!(blob.is_none(), "{op}: no blob");
    assert_eq!(frame.id, 7, "{op}: request id");
    assert!(matches!(frame.kind, Kind::Res), "{op}: kind res");
    assert_eq!(frame.op, op);
    assert_eq!(frame.rev, None, "{op}: no revision");
    assert_eq!(
        frame.payload,
        json!({ "error": format!("unknown workspace: {hint}"), "code": "unknown_workspace" }),
        "{op}: payload"
    );
    assert_eq!(
        frame.payload.to_string(),
        format!(r#"{{"code":"unknown_workspace","error":"unknown workspace: {}"}}"#, hint.replace('"', "\\\"")),
        "{op}: payload bytes"
    );
}

const NOSUCH: &str = "Some(\"ws-nosuch\")";

macro_rules! unknown_workspace_case {
    ($name:ident, $op:expr, $handler:path, $payload:expr $(, $conn:expr)?) => {
        #[tokio::test]
        async fn $name() {
            let out = $handler(7, $payload, &Session::new(), &Workspaces::new() $(, $conn)?)
                .await
                .expect("handler answers");
            assert_unknown_workspace(out, $op, NOSUCH);
        }
    };
}

unknown_workspace_case!(
    tree_root,
    "tree.root",
    crate::files::tree_ops::handle_tree_root,
    json!({ "mode": "files", "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    tree_children,
    "tree.children",
    crate::files::tree_ops::handle_tree_children,
    json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    nav_toggle_hidden,
    "nav.toggle_hidden",
    crate::files::tree_ops::handle_nav_toggle_hidden,
    json!({ "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    preview_get,
    "preview.get",
    crate::files::preview::handle_preview_get,
    json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    preview_set_scale,
    "preview.set_scale",
    crate::files::preview::scale::handle_preview_set_scale,
    json!({ "node_id": "x", "physical_scale": null, "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    image_crop,
    "image.crop",
    crate::files::preview::crop::handle_image_crop,
    json!({ "node_id": "x", "x": 0, "y": 0, "w": 1, "h": 1, "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    concept_read,
    "concept.read",
    crate::files::concept_ops::handle_concept_read,
    json!({ "target": "t", "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    concept_write,
    "concept.write",
    crate::files::concept_ops::handle_concept_write,
    json!({ "target": "t", "content": "c", "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    concept_list,
    "concept.list",
    crate::files::concept_ops::handle_concept_list,
    json!({ "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    file_read,
    "file.read",
    crate::files::io_ops::handle_file_read,
    json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    file_write,
    "file.write",
    crate::files::io_ops::handle_file_write,
    json!({ "node_id": "x", "content": "c", "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    file_delete,
    "file.delete",
    crate::files::io_ops::handle_file_delete,
    json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    dir_create,
    "dir.create",
    crate::files::io_ops::handle_dir_create,
    json!({ "node_id": "x", "workspace_id": "ws-nosuch" })
);
unknown_workspace_case!(
    repl_eval,
    "repl.eval",
    crate::sidecars::repl::ops::handle_repl_eval,
    json!({ "workspace_id": "ws-nosuch" }),
    &Default::default()
);
unknown_workspace_case!(
    repl_run_file,
    "repl.run_file",
    crate::sidecars::repl::ops::handle_repl_run_file,
    json!({ "workspace_id": "ws-nosuch" }),
    &Default::default()
);
unknown_workspace_case!(
    repl_interrupt,
    "repl.interrupt",
    crate::sidecars::repl::ops::handle_repl_interrupt,
    json!({ "workspace_id": "ws-nosuch" }),
    &Default::default()
);
unknown_workspace_case!(
    kernel_request,
    "kernel.request",
    crate::sidecars::ops::handle_kernel_request,
    json!({ "kernel_op": "k", "workspace_id": "ws-nosuch" })
);

#[tokio::test]
async fn tree_root_without_a_hint_prints_none() {
    let out = crate::files::tree_ops::handle_tree_root(
        7,
        json!({ "mode": "files" }),
        &Session::new(),
        &Workspaces::new(),
    )
    .await
    .expect("handler answers");
    assert_unknown_workspace(out, "tree.root", "None");
}

#[tokio::test]
async fn file_read_without_a_hint_prints_none() {
    let out = crate::files::io_ops::handle_file_read(
        7,
        json!({ "node_id": "x" }),
        &Session::new(),
        &Workspaces::new(),
    )
    .await
    .expect("handler answers");
    assert_unknown_workspace(out, "file.read", "None");
}
