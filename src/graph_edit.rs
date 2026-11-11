//! Nonserialized multi-selection and group positioning for the graph editor.
use gaia::terrain::Graph;
use std::collections::BTreeSet;

/// Ctrl/Shift click toggles membership. A plain click selects one card.
/// At least one primary node stays selected so the Inspector remains usable.
pub fn select(set: &mut BTreeSet<u64>, id: u64, additive: bool) {
    if !additive {
        set.clear();
        set.insert(id);
    } else if set.len() > 1 && set.contains(&id) {
        set.remove(&id);
    } else {
        set.insert(id);
    }
}

/// Dragging one selected node moves all selected positions exactly once.
/// No movement or dirty flag is required for zero-motion interactions.
pub fn move_group(
    graph: &mut Graph, selection: &BTreeSet<u64>, motion: [f32; 2],
) -> bool {
    if motion == [0.0, 0.0] || !motion.iter().all(|v| v.is_finite()) {
        return false;
    }
    let mut changed = false;
    for node in &mut graph.nodes {
        if selection.contains(&node.id) {
            node.position[0] += motion[0];
            node.position[1] += motion[1];
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use gaia::terrain::Project;

    #[test]
    fn modifiers_select_and_toggle_nodes_without_duplicate_entries() {
        let mut ids = BTreeSet::from([1]);
        select(&mut ids, 2, true);
        select(&mut ids, 3, true);
        assert_eq!(ids, BTreeSet::from([1, 2, 3]));
        select(&mut ids, 2, true);
        assert_eq!(ids, BTreeSet::from([1, 3]));
        select(&mut ids, 3, false);
        assert_eq!(ids, BTreeSet::from([3]));
        select(&mut ids, 3, true);
        assert_eq!(ids, BTreeSet::from([3]));
    }

    #[test]
    fn group_drag_updates_only_selected_nodes_and_has_no_double_delta() {
        let mut graph = Project::default().graph;
        let before = graph.clone();
        let group = BTreeSet::from([1, 3]);
        assert!(move_group(&mut graph, &group, [28.0, -12.0]));
        for node in &graph.nodes {
            let old = before.get(node.id).unwrap();
            if group.contains(&node.id) {
                assert_eq!(node.position, [old.position[0] + 28.0, old.position[1] - 12.0]);
            } else {
                assert_eq!(node.position, old.position);
            }
        }
        assert!(!move_group(&mut graph, &group, [0.0, 0.0]));
        assert!(graph.evaluate(32).is_ok());
    }
}
