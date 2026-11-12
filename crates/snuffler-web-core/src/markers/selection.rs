//! Compact selection independent of marker content and its derived indexes.

use std::collections::HashSet;

use super::MarkerId;

pub(super) enum Selection {
    Only(HashSet<MarkerId>),
    AllExcept(HashSet<MarkerId>),
}

impl Default for Selection {
    fn default() -> Self {
        Self::Only(HashSet::new())
    }
}

impl Selection {
    pub(super) fn contains(&self, id: MarkerId) -> bool {
        match self {
            Self::Only(ids) => ids.contains(&id),
            Self::AllExcept(ids) => !ids.contains(&id),
        }
    }

    pub(super) fn set(&mut self, id: MarkerId, selected: bool) -> bool {
        match self {
            Self::Only(ids) if selected => ids.insert(id),
            Self::AllExcept(ids) if !selected => ids.insert(id),
            Self::Only(ids) | Self::AllExcept(ids) => ids.remove(&id),
        }
    }

    pub(super) fn remove(&mut self, id: MarkerId) {
        match self {
            Self::Only(ids) | Self::AllExcept(ids) => {
                ids.remove(&id);
            }
        }
    }

    pub(super) fn count(&self, total: usize) -> usize {
        match self {
            Self::Only(ids) => ids.len(),
            Self::AllExcept(ids) => total.saturating_sub(ids.len()),
        }
    }
}
