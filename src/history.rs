//! Bounded, in-memory undo/redo for complete terrain project edits.
//! A pointer gesture (slider drag, canvas move) produces one history entry.
use gaia::terrain::Project;

const MAX_UNDO: usize = 64;

#[derive(Default)]
pub struct EditHistory {
    past: Vec<Project>,
    future: Vec<Project>,
    pointer_edit_active: bool,
}

impl EditHistory {
    pub fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
        self.pointer_edit_active = false;
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    /// Called once after a UI frame. Only project data changes count as edits;
    /// selection, orbit, preview, and export do not enter the undo history.
    pub fn record_frame(&mut self, before: Project, after: &Project, pointer_down: bool) {
        let changed = serde_json::to_vec(&before).expect("Project must serialize")
            != serde_json::to_vec(after).expect("Project must serialize");
        if changed && !self.pointer_edit_active {
            if self.past.len() == MAX_UNDO {
                self.past.remove(0);
            }
            self.past.push(before);
            self.future.clear();
        }
        self.pointer_edit_active = pointer_down && (changed || self.pointer_edit_active);
    }

    pub fn undo(&mut self, current: &mut Project) -> bool {
        let Some(previous) = self.past.pop() else { return false; };
        self.future.push(std::mem::replace(current, previous));
        self.pointer_edit_active = false;
        true
    }

    pub fn redo(&mut self, current: &mut Project) -> bool {
        let Some(next) = self.future.pop() else { return false; };
        self.past.push(std::mem::replace(current, next));
        self.pointer_edit_active = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_restores_projects_and_clears_redo_on_new_edit() {
        let mut history = EditHistory::default();
        let original = Project::default();
        let mut project = original.clone();
        project.resolution = 256;
        history.record_frame(original.clone(), &project, false);
        assert!(history.can_undo());
        assert!(history.undo(&mut project));
        assert_eq!(project.resolution, 128);
        assert!(history.can_redo());
        assert!(history.redo(&mut project));
        assert_eq!(project.resolution, 256);
        assert!(history.undo(&mut project));

        let before = project.clone();
        project.resolution = 512;
        history.record_frame(before, &project, false);
        assert!(!history.can_redo());
        assert!(history.undo(&mut project));
        assert_eq!(project.resolution, 128);
    }

    #[test]
    fn pointer_drag_is_one_undo_entry() {
        let mut history = EditHistory::default();
        let mut project = Project::default();
        let before = project.clone();
        project.resolution = 256;
        history.record_frame(before, &project, true);
        let intermediate = project.clone();
        project.resolution = 512;
        history.record_frame(intermediate, &project, true);
        history.record_frame(project.clone(), &project, false);
        assert!(history.undo(&mut project));
        assert_eq!(project.resolution, 128);
        assert!(!history.can_undo());
    }

    #[test]
    fn untouched_frame_and_clearing_do_not_create_history() {
        let mut history = EditHistory::default();
        let project = Project::default();
        history.record_frame(project.clone(), &project, false);
        assert!(!history.can_undo());
        let mut changed = project.clone();
        changed.resolution = 256;
        history.record_frame(project, &changed, false);
        history.clear();
        assert!(!history.can_undo());
        assert!(!history.can_redo());
    }
}
