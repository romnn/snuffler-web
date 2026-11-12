//! Stable marker ownership and indexed time queries.

use std::cell::RefCell;
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::rc::Rc;

use super::Marker;
use super::scope::{ScopeIndex, TrackMarkers};
use super::selection::Selection;
use crate::bridge::ViewTrack;

/// Default producer-side marker response bound.
pub const DEFAULT_MARKER_BUDGET: u32 = 20_000;
/// Largest marker response the UI permits a caller to request.
pub const MAX_MARKER_BUDGET: u32 = 100_000;

/// Validated upper bound carried by every staged marker result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkerBudget(NonZeroU32);

impl MarkerBudget {
    /// Returns the normal application marker budget.
    #[must_use]
    pub fn standard() -> Self {
        Self(NonZeroU32::new(DEFAULT_MARKER_BUDGET).unwrap_or(NonZeroU32::MIN))
    }

    /// Interprets a producer-echoed budget, falling back to the standard bound when the
    /// echo is zero or exceeds the calibrated hard cap.
    /// Nothing lets a user pick a budget, so an invalid echo is a producer bug to absorb,
    /// not an error for a caller to route.
    #[must_use]
    pub fn from_wire(value: u32) -> Self {
        NonZeroU32::new(value)
            .filter(|value| value.get() <= MAX_MARKER_BUDGET)
            .map_or_else(Self::standard, Self)
    }

    /// Returns the protobuf value sent to Python.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl From<MarkerBudget> for snuffler_web_proto::MarkerBudget {
    fn from(budget: MarkerBudget) -> Self {
        Self {
            max_markers: budget.get(),
        }
    }
}

/// Fully decoded markers ready for one atomic origin replacement.
pub struct StagedMarkers {
    /// Markers already bounded by the Python producer.
    pub markers: Vec<Marker>,
    /// Derived origin to replace, or `None` for accumulating user/catalog data.
    pub origin: Option<String>,
    /// Count omitted by the producer.
    pub dropped: u64,
    /// Bound used by the producer.
    pub budget: MarkerBudget,
}

/// Result that forces callers to surface producer truncation.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerCommitOutcome {
    /// Every produced marker was installed.
    Committed {
        /// Number installed.
        added: usize,
    },
    /// A bounded prefix/ranking was installed and omissions must be shown.
    Truncated {
        /// Number installed.
        added: usize,
        /// Number omitted before protobuf serialization.
        dropped: u64,
        /// Producer bound.
        budget: MarkerBudget,
    },
}

/// Stable identity for a marker slot.
///
/// Removing a marker increments its slot generation before the slot can be reused, so a stale
/// identity can never resolve to a later marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MarkerId {
    slot: u32,
    slot_generation: u32,
}

/// A bounded marker query around one time coordinate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerCandidates {
    /// Every candidate fits within the interaction budget.
    Exact(Vec<MarkerId>),
    /// More marker endpoints occupy the coordinate than interaction can inspect safely.
    Overloaded {
        /// Number of endpoint candidates in the queried tolerance window.
        count: usize,
    },
}

/// One identity slot: where its marker sits now, and how often it was reused.
#[derive(Clone, Copy, Debug)]
struct Slot {
    generation: u32,
    /// Insertion-order index of the marker, or `None` while the slot is free.
    index: Option<usize>,
}

/// Time-sorted views of the markers, rebuilt lazily once per store generation.
#[derive(Default)]
struct TimeIndex {
    /// The store generation this index was built from.
    store_generation: u64,
    /// Every marker's start time, ascending.
    starts: Vec<(f64, MarkerId)>,
    /// Every marker's start and (for spans) end time, ascending; the hit-test targets.
    endpoints: Vec<(f64, MarkerId)>,
    /// The longest marker span, which widens a start-time query to catch spans that
    /// began before the window.
    max_span: f64,
    first_event: Option<MarkerId>,
    events: Vec<MarkerId>,
    earliest_marker_start: f64,
    latest_starting_marker_end: f64,
    active_hash: Option<String>,
}

#[derive(Default)]
struct SelectionSummary {
    revision: u64,
    selected: usize,
    single_selected: Option<MarkerId>,
    selected_event: Option<MarkerId>,
}

/// The index range of `entries` whose times lie in `lower..=upper`.
fn inclusive_time_range(
    entries: &[(f64, MarkerId)],
    lower: f64,
    upper: f64,
) -> std::ops::Range<usize> {
    let begin = entries.partition_point(|(time, _)| time.total_cmp(&lower).is_lt());
    let end = entries.partition_point(|(time, _)| !time.total_cmp(&upper).is_gt());
    begin..end
}

/// Cached aggregates used by marker rendering without rescanning the store.
#[derive(Clone, Debug)]
pub struct MarkerSummary {
    /// Number of selected markers.
    pub selected: usize,
    /// Identity of the sole selected marker, or `None` for an empty or multiple selection.
    pub single_selected: Option<MarkerId>,
    /// First event in insertion order, used before an event is selected.
    pub first_event: Option<MarkerId>,
    /// The sole selected event, or `None` when zero or several events are selected.
    pub selected_event: Option<MarkerId>,
    /// Earliest marker start.
    pub earliest_marker_start: f64,
    /// End of the latest-starting marker.
    pub latest_starting_marker_end: f64,
    /// Hash of the active event, when one exists.
    pub active_hash: Option<String>,
}

/// Owns markers in insertion order while preserving identity across compaction.
pub struct MarkerStore {
    markers: Vec<Marker>,
    ids: Vec<MarkerId>,
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    store_generation: u64,
    revision: u64,
    selection: Selection,
    selection_summary: RefCell<SelectionSummary>,
    scope_index: RefCell<ScopeIndex>,
    time_index: RefCell<TimeIndex>,
}

impl Default for MarkerStore {
    fn default() -> Self {
        Self {
            markers: Vec::new(),
            ids: Vec::new(),
            slots: Vec::new(),
            free_slots: Vec::new(),
            store_generation: 1,
            revision: 1,
            selection: Selection::default(),
            selection_summary: RefCell::new(SelectionSummary::default()),
            scope_index: RefCell::new(ScopeIndex::default()),
            time_index: RefCell::new(TimeIndex::default()),
        }
    }
}

impl MarkerStore {
    /// Returns the number of stored markers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.markers.len()
    }

    /// Reports whether the store contains no markers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.markers.is_empty()
    }

    /// Returns the generation that changes whenever marker content may have changed.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.revision
    }

    /// Returns the content generation, unchanged by selection-only edits.
    #[must_use]
    pub fn content_generation(&self) -> u64 {
        self.store_generation
    }

    /// Reports whether a current marker participates in selection-based operations.
    #[must_use]
    pub fn is_selected(&self, id: MarkerId) -> bool {
        self.index_of(id).is_some() && self.selection.contains(id)
    }

    /// Changes one current marker's selection without invalidating content indexes.
    pub fn set_selected(&mut self, id: MarkerId, selected: bool) {
        if self.index_of(id).is_some() && self.selection.set(id, selected) {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Selects all current markers using an empty exclusion set.
    pub fn select_all(&mut self) {
        self.selection = Selection::AllExcept(HashSet::new());
        self.revision = self.revision.wrapping_add(1);
    }

    /// Clears selection without walking the markers.
    pub fn deselect_all(&mut self) {
        self.selection = Selection::default();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Replaces the selection with one current marker, or clears it for a stale id.
    pub fn select_only(&mut self, id: MarkerId) {
        self.deselect_all();
        self.set_selected(id, true);
    }

    /// Replaces selection with markers satisfying a content predicate.
    pub fn select_where(&mut self, mut select: impl FnMut(&Marker) -> bool) {
        let excluded: HashSet<_> = self
            .iter_with_ids()
            .filter_map(|(id, marker)| (!select(marker)).then_some(id))
            .collect();
        self.selection = if excluded.len() <= self.len() / 2 {
            Selection::AllExcept(excluded)
        } else {
            Selection::Only(
                self.ids
                    .iter()
                    .copied()
                    .filter(|id| !excluded.contains(id))
                    .collect(),
            )
        };
        self.revision = self.revision.wrapping_add(1);
    }

    /// Adds matching markers without materializing an intermediate identity list.
    pub fn add_selection_where(&mut self, mut select: impl FnMut(&Marker) -> bool) {
        for (&id, marker) in self.ids.iter().zip(&self.markers) {
            if select(marker) {
                self.selection.set(id, true);
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Iterates over selected markers in insertion order.
    pub fn selected(&self) -> impl Iterator<Item = &Marker> {
        self.iter_with_ids()
            .filter(|(id, _)| self.selection.contains(*id))
            .map(|(_, marker)| marker)
    }

    /// Edits selected markers, invalidating content indexes once for the batch.
    pub fn selected_mut(&mut self) -> impl Iterator<Item = &mut Marker> {
        self.bump_generation();
        let selection = &self.selection;
        self.ids
            .iter()
            .zip(&mut self.markers)
            .filter_map(move |(&id, marker)| selection.contains(id).then_some(marker))
    }

    /// Returns the stable identity at an insertion-order index.
    #[must_use]
    pub fn id_at(&self, index: usize) -> Option<MarkerId> {
        self.ids.get(index).copied()
    }

    /// Returns cached channel membership for a row, including grouped components.
    #[must_use]
    pub fn for_track(&self, track: &ViewTrack) -> Rc<TrackMarkers> {
        self.scope_members(track, &track.loc, true)
    }

    /// Returns cached membership for a concrete span within a row.
    #[must_use]
    pub fn for_trace(&self, track: &ViewTrack, loc: &str) -> Rc<TrackMarkers> {
        self.scope_members(track, loc, false)
    }

    fn scope_members(&self, track: &ViewTrack, loc: &str, grouped: bool) -> Rc<TrackMarkers> {
        let mut index = self.scope_index.borrow_mut();
        if index.generation != self.store_generation {
            *index = ScopeIndex::build(self.store_generation, self.iter_with_ids());
        }
        index.matching(track, loc, grouped)
    }

    /// Removes the current selection while preserving surviving identities.
    pub fn remove_selected(&mut self) {
        let selection = std::mem::take(&mut self.selection);
        self.retain_with_ids(|id, _| !selection.contains(id));
    }

    /// Borrows all markers in insertion order.
    #[must_use]
    pub fn as_slice(&self) -> &[Marker] {
        &self.markers
    }

    /// Borrows all markers mutably and invalidates every derived index once.
    pub fn as_mut_slice(&mut self) -> &mut [Marker] {
        self.bump_generation();
        &mut self.markers
    }

    /// Iterates over markers in insertion order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Marker> {
        self.markers.iter()
    }

    /// Iterates mutably and invalidates every derived index once.
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = &mut Marker> {
        self.bump_generation();
        self.markers.iter_mut()
    }

    /// Iterates over stable identities and their markers in insertion order.
    pub fn iter_with_ids(&self) -> impl ExactSizeIterator<Item = (MarkerId, &Marker)> {
        self.ids.iter().copied().zip(self.markers.iter())
    }

    /// Returns a marker by insertion-order index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Marker> {
        self.markers.get(index)
    }

    /// Returns a mutable marker by insertion-order index and invalidates derived indexes.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut Marker> {
        self.bump_generation();
        self.markers.get_mut(index)
    }

    /// Resolves a stable identity to its current marker.
    #[must_use]
    pub fn by_id(&self, id: MarkerId) -> Option<&Marker> {
        let index = self.index_of(id)?;
        self.markers.get(index)
    }

    /// Resolves a stable identity to its current insertion-order index.
    fn index_of(&self, id: MarkerId) -> Option<usize> {
        let slot = self.slots.get(usize::try_from(id.slot).ok()?)?;
        (slot.generation == id.slot_generation)
            .then_some(slot.index)
            .flatten()
    }

    /// Mutates one marker and advances the generation only when its value changed.
    pub fn edit<R>(&mut self, id: MarkerId, edit: impl FnOnce(&mut Marker) -> R) -> Option<R> {
        let index = self.index_of(id)?;
        let before = self.markers.get(index)?.clone();
        let result = edit(self.markers.get_mut(index)?);
        if self.markers.get(index) != Some(&before) {
            self.bump_generation();
        }
        Some(result)
    }

    /// Mutates one marker with the closure itself reporting whether anything
    /// changed, skipping [`Self::edit`]'s clone-and-compare — the per-frame
    /// path for widget rows, where every mutation site is explicit.
    /// Returns whether the marker existed and reported a change.
    pub fn edit_with_change_flag(
        &mut self,
        id: MarkerId,
        edit: impl FnOnce(&mut Marker) -> bool,
    ) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let Some(marker) = self.markers.get_mut(index) else {
            return false;
        };
        if edit(marker) {
            self.bump_generation();
            return true;
        }
        false
    }

    /// Inserts a marker and returns its stable identity.
    pub fn insert(&mut self, marker: Marker) -> MarkerId {
        let index = self.markers.len();
        let id = self
            .free_slots
            .pop()
            .and_then(|slot_index| {
                let slot = self.slots.get_mut(usize::try_from(slot_index).ok()?)?;
                slot.index = Some(index);
                Some(MarkerId {
                    slot: slot_index,
                    slot_generation: slot.generation,
                })
            })
            .unwrap_or_else(|| {
                let slot_index = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
                self.slots.push(Slot {
                    generation: 0,
                    index: Some(index),
                });
                MarkerId {
                    slot: slot_index,
                    slot_generation: 0,
                }
            });
        self.markers.push(marker);
        self.ids.push(id);
        self.selection.set(id, false);
        self.bump_generation();
        id
    }

    /// Applies one producer result as an atomic derived-origin replacement.
    pub fn commit(&mut self, staged: StagedMarkers) -> MarkerCommitOutcome {
        let StagedMarkers {
            mut markers,
            origin,
            mut dropped,
            budget,
        } = staged;
        if let Some(origin) = origin.as_deref() {
            self.retain(|marker| marker.origin.as_deref() != Some(origin));
        }
        let available = usize::try_from(MAX_MARKER_BUDGET)
            .unwrap_or(usize::MAX)
            .saturating_sub(self.len());
        let allowed = available.min(usize::try_from(budget.get()).unwrap_or(usize::MAX));
        if markers.len() > allowed {
            if allowed > 0
                && !markers
                    .get(..allowed)
                    .unwrap_or_default()
                    .iter()
                    .any(|marker| marker.active)
                && let Some(active) = markers.iter().position(|marker| marker.active)
            {
                markers.swap(allowed - 1, active);
            }
            let omitted = markers.len().saturating_sub(allowed);
            dropped = dropped.saturating_add(u64::try_from(omitted).unwrap_or(u64::MAX));
            markers.truncate(allowed);
        }
        let added = markers.len();
        for marker in markers {
            self.insert(marker);
        }
        if dropped == 0 {
            MarkerCommitOutcome::Committed { added }
        } else {
            MarkerCommitOutcome::Truncated {
                added,
                dropped,
                budget,
            }
        }
    }

    /// Retains markers that satisfy `keep` without changing surviving identities.
    pub fn retain(&mut self, mut keep: impl FnMut(&Marker) -> bool) {
        self.retain_with_ids(|_, marker| keep(marker));
    }

    fn retain_with_ids(&mut self, mut keep: impl FnMut(MarkerId, &Marker) -> bool) {
        let old_markers = std::mem::take(&mut self.markers);
        let old_ids = std::mem::take(&mut self.ids);
        for (marker, id) in old_markers.into_iter().zip(old_ids) {
            if keep(id, &marker) {
                let index = self.markers.len();
                if let Some(slot) = usize::try_from(id.slot)
                    .ok()
                    .and_then(|slot| self.slots.get_mut(slot))
                {
                    slot.index = Some(index);
                }
                self.markers.push(marker);
                self.ids.push(id);
            } else {
                self.release(id);
            }
        }
        self.bump_generation();
    }

    /// Removes every marker and invalidates all outstanding identities.
    pub fn clear(&mut self) {
        let ids = std::mem::take(&mut self.ids);
        self.markers.clear();
        self.selection = Selection::default();
        for id in ids {
            self.release(id);
        }
        self.bump_generation();
    }

    /// Counts marker starts in the renderer's widened overload window.
    #[must_use]
    pub fn overload_count(&self, tmin: f64, tmax: f64) -> usize {
        self.ensure_time_index();
        let index = self.time_index.borrow();
        let lower = tmin - index.max_span;
        inclusive_time_range(&index.starts, lower, tmax).len()
    }

    /// Returns markers overlapping a time window under a bounded candidate contract.
    #[must_use]
    pub fn overlapping(&self, tmin: f64, tmax: f64, max_candidates: usize) -> MarkerCandidates {
        self.ensure_time_index();
        let index = self.time_index.borrow();
        let lower = tmin - index.max_span;
        let range = inclusive_time_range(&index.starts, lower, tmax);
        let candidates = index.starts.get(range).unwrap_or_default();
        let mut ids = Vec::with_capacity(candidates.len().min(max_candidates));
        let mut count = 0;
        for &(_, id) in candidates {
            if self
                .by_id(id)
                .is_some_and(|marker| marker.tmin < tmax && tmin < marker.tmax)
            {
                count += 1;
                if ids.len() < max_candidates {
                    ids.push(id);
                }
            }
        }
        if count > max_candidates {
            MarkerCandidates::Overloaded { count }
        } else {
            ids.sort_by_key(|id| self.index_of(*id).unwrap_or(usize::MAX));
            MarkerCandidates::Exact(ids)
        }
    }

    /// Returns generation-cached aggregates for the renderer and marker table.
    #[must_use]
    pub fn summary(&self) -> MarkerSummary {
        self.ensure_time_index();
        let index = self.time_index.borrow();
        let mut selection = self.selection_summary.borrow_mut();
        if selection.revision != self.revision {
            let selected = self.selection.count(self.len());
            let single_selected = (selected == 1)
                .then(|| {
                    self.ids
                        .iter()
                        .copied()
                        .find(|id| self.selection.contains(*id))
                })
                .flatten();
            let mut events = index
                .events
                .iter()
                .copied()
                .filter(|id| self.selection.contains(*id));
            let first = events.next();
            let selected_event = if events.next().is_none() { first } else { None };
            *selection = SelectionSummary {
                revision: self.revision,
                selected,
                single_selected,
                selected_event,
            };
        }
        MarkerSummary {
            selected: selection.selected,
            single_selected: selection.single_selected,
            first_event: index.first_event,
            selected_event: selection.selected_event,
            earliest_marker_start: index.earliest_marker_start,
            latest_starting_marker_end: index.latest_starting_marker_end,
            active_hash: index.active_hash.clone(),
        }
    }

    /// Returns the markers whose endpoints fall inside a time tolerance.
    #[must_use]
    pub fn candidates_near(
        &self,
        time: f64,
        tolerance: f64,
        max_candidates: usize,
    ) -> MarkerCandidates {
        self.ensure_time_index();
        let index = self.time_index.borrow();
        let lower = time - tolerance.abs();
        let upper = time + tolerance.abs();
        let range = inclusive_time_range(&index.endpoints, lower, upper);
        let candidates = index.endpoints.get(range).unwrap_or_default();
        let candidate_count = candidates.len();
        if candidate_count > max_candidates {
            return MarkerCandidates::Overloaded {
                count: candidate_count,
            };
        }

        let mut seen = HashSet::with_capacity(candidate_count);
        let mut ids = Vec::with_capacity(candidate_count);
        for &(_, id) in candidates {
            if seen.insert(id) {
                ids.push(id);
            }
        }
        ids.sort_by_key(|id| self.index_of(*id).unwrap_or(usize::MAX));
        MarkerCandidates::Exact(ids)
    }

    fn bump_generation(&mut self) {
        self.store_generation = self.store_generation.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
    }

    fn release(&mut self, id: MarkerId) {
        self.selection.remove(id);
        let Some(slot) = usize::try_from(id.slot)
            .ok()
            .and_then(|slot| self.slots.get_mut(slot))
        else {
            return;
        };
        slot.index = None;
        slot.generation = slot.generation.wrapping_add(1);
        self.free_slots.push(id.slot);
    }

    fn ensure_time_index(&self) {
        if self.time_index.borrow().store_generation == self.store_generation {
            return;
        }
        let mut starts = Vec::with_capacity(self.markers.len());
        let mut endpoints = Vec::with_capacity(self.markers.len().saturating_mul(2));
        let mut max_span = 0.0_f64;
        let mut first_event = None;
        let mut events = Vec::new();
        let mut earliest_marker_start = f64::INFINITY;
        let mut latest_start = f64::NEG_INFINITY;
        let mut latest_starting_marker_end = f64::NEG_INFINITY;
        let mut active_hash = None;
        for (id, marker) in self.iter_with_ids() {
            starts.push((marker.tmin, id));
            endpoints.push((marker.tmin, id));
            if marker.tmax > marker.tmin {
                endpoints.push((marker.tmax, id));
            }
            max_span = max_span.max(marker.tmax - marker.tmin);
            if marker.is_event {
                first_event.get_or_insert(id);
                events.push(id);
            }
            earliest_marker_start = earliest_marker_start.min(marker.tmin);
            if marker.tmin > latest_start {
                latest_start = marker.tmin;
                latest_starting_marker_end = marker.tmax;
            }
            if marker.active {
                active_hash.clone_from(&marker.event_hash);
            }
        }
        starts.sort_by(|left, right| left.0.total_cmp(&right.0));
        endpoints.sort_by(|left, right| left.0.total_cmp(&right.0));
        *self.time_index.borrow_mut() = TimeIndex {
            store_generation: self.store_generation,
            starts,
            endpoints,
            max_span,
            first_event,
            events,
            earliest_marker_start,
            latest_starting_marker_end,
            active_hash,
        };
    }
}

impl<'a> IntoIterator for &'a MarkerStore {
    type Item = &'a Marker;
    type IntoIter = std::slice::Iter<'a, Marker>;

    fn into_iter(self) -> Self::IntoIter {
        self.markers.iter()
    }
}

impl<'a> IntoIterator for &'a mut MarkerStore {
    type Item = &'a mut Marker;
    type IntoIter = std::slice::IterMut<'a, Marker>;

    fn into_iter(self) -> Self::IntoIter {
        self.bump_generation();
        self.markers.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::{self, OptionExt as _};
    use std::rc::Rc;

    use super::Selection;
    use super::{
        MAX_MARKER_BUDGET, MarkerBudget, MarkerCandidates, MarkerCommitOutcome, MarkerStore,
        StagedMarkers,
    };
    use crate::bridge::ViewTrack;
    use crate::markers::Marker;

    #[test]
    fn all_except_selection_is_compact_and_does_not_rebuild_content_indexes() -> eyre::Result<()> {
        let mut store = MarkerStore::default();
        for index in 0..MAX_MARKER_BUDGET {
            store.insert(Marker::new(f64::from(index), f64::from(index), 0));
        }
        let track = ViewTrack::default();
        let membership = store.for_track(&track);
        let generation = store.content_generation();
        let first = store.id_at(0).ok_or_eyre("missing first marker")?;
        store.select_all();
        assert!(matches!(&store.selection, Selection::AllExcept(ids) if ids.is_empty()));
        let total = usize::try_from(MAX_MARKER_BUDGET)?;
        assert_eq!(store.summary().selected, total);
        store.set_selected(first, false);
        assert!(matches!(&store.selection, Selection::AllExcept(ids) if ids.len() == 1));
        assert_eq!(store.summary().selected, total - 1);
        assert_eq!(store.content_generation(), generation);
        assert!(Rc::ptr_eq(&membership, &store.for_track(&track)));
        let inserted = store.insert(Marker::new(-1.0, -1.0, 0));
        assert!(!store.is_selected(inserted));
        store.select_only(first);
        assert_eq!(store.summary().single_selected, Some(first));
        store.remove_selected();
        assert!(!store.is_selected(first));
        let reused = store.insert(Marker::new(-2.0, -2.0, 0));
        assert_ne!(first, reused);
        assert!(!store.is_selected(reused));
        store.set_selected(first, true);
        assert_eq!(store.summary().selected, 0);
        Ok(())
    }

    #[test]
    fn channel_membership_matches_globs_and_grouping_and_refreshes_after_edits() -> eyre::Result<()>
    {
        let mut store = MarkerStore::default();
        store.insert(Marker::new(0.0, 1.0, 0));
        for (net, sta, loc, cha) in [
            ("XX", "ONE", "", "BHZ"),
            ("YY", "TWO", "10", "BHN"),
            ("X?", "O*", "*", "BH[ZNE]"),
            ("*", "*", "", "BH[!Z]"),
            ("XX", "ONE", "10", "BHZ"),
        ] {
            let mut marker = Marker::new(0.0, 1.0, 0);
            marker.nslc_ids.push((
                net.to_owned(),
                sta.to_owned(),
                loc.to_owned(),
                cha.to_owned(),
            ));
            store.insert(marker);
        }
        for net in ["XX", "YY", "*"] {
            for sta in ["ONE", "TWO", "*"] {
                for loc in ["", "10", "*"] {
                    for cha in ["BHZ", "BHN", "*"] {
                        let track = ViewTrack {
                            net: net.to_owned(),
                            sta: sta.to_owned(),
                            loc: loc.to_owned(),
                            cha: cha.to_owned(),
                            ..ViewTrack::default()
                        };
                        let indexed = store.for_track(&track);
                        for (id, marker) in store.iter_with_ids() {
                            let expected = marker.nslc_ids.is_empty()
                                || marker
                                    .nslc_ids
                                    .iter()
                                    .any(|nslc| super::super::marker_matches_track(nslc, &track));
                            assert_eq!(indexed.contains(id), expected, "{net}.{sta}.{loc}.{cha}");
                        }
                    }
                }
            }
        }
        let track = ViewTrack {
            net: "XX".to_owned(),
            sta: "ONE".to_owned(),
            loc: "*".to_owned(),
            cha: "BHZ".to_owned(),
            ..ViewTrack::default()
        };
        let id = store.id_at(1).ok_or_eyre("missing scoped marker")?;
        assert!(store.for_track(&track).contains(id));
        assert!(!store.for_trace(&track, "10").contains(id));
        assert!(store.edit(id, |marker| marker.nslc_ids.clear()).is_some());
        assert!(store.for_trace(&track, "10").contains(id));
        store.retain(|marker| !marker.nslc_ids.is_empty());
        assert!(!store.for_track(&track).contains(id));
        Ok(())
    }

    /// Surviving markers keep their identity when a preceding slot is removed and reused.
    #[test]
    fn marker_ids_survive_compaction_and_reject_slot_reuse() {
        let mut store = MarkerStore::default();
        let removed = store.insert(Marker::new(1.0, 1.0, 0));
        let survivor = store.insert(Marker::new(2.0, 2.0, 0));

        store.retain(|marker| marker.tmin > 1.0);
        let replacement = store.insert(Marker::new(3.0, 3.0, 0));

        assert!(store.by_id(removed).is_none());
        assert!(
            store
                .by_id(survivor)
                .is_some_and(|marker| { (marker.tmin - 2.0).abs() < f64::EPSILON })
        );
        assert_ne!(replacement, removed);
    }

    /// Endpoint queries stay bounded even when every marker occupies one timestamp.
    #[test]
    fn pathological_endpoint_query_reports_overload() {
        let mut store = MarkerStore::default();
        for _ in 0..300 {
            store.insert(Marker::new(10.0, 10.0, 0));
        }

        assert_eq!(
            store.candidates_near(10.0, 0.1, 256),
            MarkerCandidates::Overloaded { count: 300 }
        );
    }

    /// The overload count uses the longest span to match the renderer's widened window.
    #[test]
    fn overload_count_matches_the_render_window() {
        let mut store = MarkerStore::default();
        store.insert(Marker::new(5.0, 10.0, 0));
        store.insert(Marker::new(9.0, 9.0, 0));
        store.insert(Marker::new(20.0, 20.0, 0));

        assert_eq!(store.overload_count(10.0, 12.0), 2);
    }

    #[test]
    fn producer_truncation_is_typed_and_origin_replacement_is_atomic() {
        let mut store = MarkerStore::default();
        let mut old = Marker::new(1.0, 1.0, 0);
        old.origin = Some("picker".to_owned());
        store.insert(old);
        store.insert(Marker::new(2.0, 2.0, 0));
        let mut replacement = Marker::new(3.0, 3.0, 0);
        replacement.origin = Some("picker".to_owned());
        let budget = MarkerBudget::from_wire(1);

        let outcome = store.commit(StagedMarkers {
            markers: vec![replacement],
            origin: Some("picker".to_owned()),
            dropped: 9,
            budget,
        });

        assert_eq!(
            outcome,
            MarkerCommitOutcome::Truncated {
                added: 1,
                dropped: 9,
                budget,
            }
        );
        assert_eq!(store.len(), 2);
        assert!(
            store
                .iter()
                .any(|marker| (marker.tmin - 2.0).abs() < f64::EPSILON)
        );
        assert!(
            store
                .iter()
                .any(|marker| (marker.tmin - 3.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn repeated_originless_commits_cannot_exceed_the_global_cap() {
        let mut store = MarkerStore::default();
        for index in 0..MAX_MARKER_BUDGET {
            store.insert(Marker::new(f64::from(index), f64::from(index), 0));
        }
        let budget = MarkerBudget::from_wire(1);

        let outcome = store.commit(StagedMarkers {
            markers: vec![Marker::new(200_000.0, 200_000.0, 0)],
            origin: None,
            dropped: 0,
            budget,
        });

        assert_eq!(
            outcome,
            MarkerCommitOutcome::Truncated {
                added: 0,
                dropped: 1,
                budget,
            }
        );
        assert_eq!(
            store.len(),
            usize::try_from(MAX_MARKER_BUDGET).unwrap_or(usize::MAX)
        );
    }
}
