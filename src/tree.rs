use std::sync::Arc;

use axum::{extract::State, Json};
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize, Default)]
pub(crate) struct TreeNode {
    name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<TreeNode>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    track_ids: Vec<String>,
}

pub(crate) async fn get_tree(State(state): State<Arc<AppState>>) -> Json<TreeNode> {
    let tracks = state.tracks.read().await;
    let mut root = TreeNode {
        name: "root".to_string(),
        children: Vec::new(),
        track_ids: Vec::new(),
    };

    for track in tracks.values() {
        let parts: Vec<&str> = track
            .relative_path
            .split('/')
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            continue;
        }
        insert_into_tree(&mut root, &parts, &track.id);
    }
    sort_tree(&mut root);
    Json(root)
}

fn insert_into_tree(node: &mut TreeNode, parts: &[&str], track_id: &str) {
    if parts.len() <= 1 {
        node.track_ids.push(track_id.to_string());
        return;
    }
    let head = parts[0];
    let rest = &parts[1..];
    let idx = match node.children.iter().position(|c| c.name == head) {
        Some(i) => i,
        None => {
            node.children.push(TreeNode {
                name: head.to_string(),
                children: Vec::new(),
                track_ids: Vec::new(),
            });
            node.children.len() - 1
        }
    };
    insert_into_tree(&mut node.children[idx], rest, track_id);
}

fn sort_tree(node: &mut TreeNode) {
    node.children.sort_by(|a, b| a.name.cmp(&b.name));
    node.track_ids.sort();
    for child in node.children.iter_mut() {
        sort_tree(child);
    }
}
