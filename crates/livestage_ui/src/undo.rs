//! Coalescing snapshot undo/redo for the project.
//!
//! Every edit marks the stack "pending". Once the operator has been idle for
//! [`COALESCE`] with no mouse button held, the pending edits are committed as
//! a single step, so dragging a fader or typing a name is one undo.

use livestage_core::Project;
use std::time::{Duration, Instant};

pub const COALESCE: Duration = Duration::from_millis(350);
const LIMIT: usize = 100;

pub struct UndoStack {
    past: Vec<Project>,
    future: Vec<Project>,
    /// The project as of the last commit.
    committed: Project,
    pending_since: Option<Instant>,
}

impl UndoStack {
    pub fn new(project: &Project) -> Self {
        Self { past: Vec::new(), future: Vec::new(), committed: project.clone(), pending_since: None }
    }

    pub fn touch(&mut self) {
        self.pending_since.get_or_insert_with(Instant::now);
    }

    pub fn has_pending(&self) -> bool {
        self.pending_since.is_some()
    }

    pub fn commit(&mut self, current: &Project) {
        if self.pending_since.take().is_none() {
            return;
        }
        let prev = std::mem::replace(&mut self.committed, current.clone());
        self.past.push(prev);
        if self.past.len() > LIMIT {
            self.past.remove(0);
        }
        self.future.clear();
    }

    /// Commits pending edits once the operator is idle.
    pub fn maybe_commit(&mut self, current: &Project, pointer_down: bool) {
        if !pointer_down && self.pending_since.is_some_and(|t| t.elapsed() >= COALESCE) {
            self.commit(current);
        }
    }

    /// Returns the project to restore.
    pub fn undo(&mut self, current: &Project) -> Option<Project> {
        self.commit(current);
        let prev = self.past.pop()?;
        self.future.push(std::mem::replace(&mut self.committed, prev.clone()));
        Some(prev)
    }

    pub fn redo(&mut self, current: &Project) -> Option<Project> {
        if self.has_pending() {
            // New edits since the undo invalidate the redo branch.
            self.commit(current);
            return None;
        }
        let next = self.future.pop()?;
        self.past.push(std::mem::replace(&mut self.committed, next.clone()));
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty() || self.has_pending()
    }
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty() && !self.has_pending()
    }

    /// Forget history (after opening / recovering a different project).
    pub fn reset(&mut self, project: &Project) {
        *self = Self::new(project);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesced_undo_redo() {
        let mut p = Project::new("A");
        let mut u = UndoStack::new(&p);
        p.name = "B".into();
        u.touch();
        p.name = "C".into(); // same step
        u.touch();
        u.commit(&p);
        p.name = "D".into();
        u.touch();
        let restored = u.undo(&p).unwrap(); // commits D, then steps back to C
        assert_eq!(restored.name, "C");
        let restored = u.undo(&restored).unwrap();
        assert_eq!(restored.name, "A");
        assert!(u.undo(&restored).is_none());
        let again = u.redo(&restored).unwrap();
        assert_eq!(again.name, "C");
        assert_eq!(u.redo(&again).unwrap().name, "D");
        assert!(!u.can_redo());
    }
}
