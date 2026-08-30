//! Stable marker ownership and indexed time queries.

use std::cell::RefCell;
use std::collections::HashSet;
use std::num::NonZeroU32;

use crate::markers::Marker;

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

    /// Interprets a producer-echoed budget, falling back to the standard bound
    /// when the echo is zero or exceeds the calibrated hard cap. Nothing lets a
    /// user pick a budget, so an invalid echo is a producer bug to absorb, not
    /// an error for a caller to route.
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
pub enum CommitOutcome {
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

#[derive(Clone, Copy, Debug)]
struct Slot {
    generation: u32,
    index: Option<usize>,
}

#[derive(Default)]
struct TimeIndex {
    store_generation: u64,
    starts: Vec<(f64, MarkerId)>,
    endpoints: Vec<(f64, MarkerId)>,
    max_span: f64,
}

/// Owns markers in insertion order while preserving identity across compaction.
pub struct MarkerStore {
    markers: Vec<Marker>,
    ids: Vec<MarkerId>,
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    store_generation: u64,
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
        self.store_generation
    }

    /// Borrows all markers in insertion order.
    #[must_use]
    pub fn as_slice(&self) -> &[Marker] {
        &self.markers
    }

    /// Borrows all markers mutably and invalidates every derived index once.
    pub fn as_mut_slice(&mut self) -> &mut [Marker] {
        self.changed();
        &mut self.markers
    }

    /// Iterates over markers in insertion order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Marker> {
        self.markers.iter()
    }

    /// Iterates mutably and invalidates every derived index once.
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = &mut Marker> {
        self.changed();
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
        self.changed();
        self.markers.get_mut(index)
    }

    /// Returns the first marker.
    #[must_use]
    pub fn first(&self) -> Option<&Marker> {
        self.markers.first()
    }

    /// Returns the first marker mutably and invalidates derived indexes.
    pub fn first_mut(&mut self) -> Option<&mut Marker> {
        self.changed();
        self.markers.first_mut()
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
            self.changed();
        }
        Some(result)
    }

    /// Mutates one marker with the closure itself reporting whether anything
    /// changed, skipping [`Self::edit`]'s clone-and-compare — the per-frame
    /// path for widget rows, where every mutation site is explicit. Returns
    /// whether the marker existed and reported a change.
    pub fn edit_reported(
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
            self.changed();
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
        self.changed();
        id
    }

    /// Applies one producer result as an atomic derived-origin replacement.
    pub fn commit(&mut self, staged: StagedMarkers) -> CommitOutcome {
        if let Some(origin) = staged.origin.as_deref() {
            self.retain(|marker| marker.origin.as_deref() != Some(origin));
        }
        let added = staged.markers.len();
        for marker in staged.markers {
            self.insert(marker);
        }
        if staged.dropped == 0 {
            CommitOutcome::Committed { added }
        } else {
            CommitOutcome::Truncated {
                added,
                dropped: staged.dropped,
                budget: staged.budget,
            }
        }
    }

    /// Retains markers that satisfy `keep` without changing surviving identities.
    pub fn retain(&mut self, mut keep: impl FnMut(&Marker) -> bool) {
        let old_markers = std::mem::take(&mut self.markers);
        let old_ids = std::mem::take(&mut self.ids);
        for (marker, id) in old_markers.into_iter().zip(old_ids) {
            if keep(&marker) {
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
        self.changed();
    }

    /// Removes every marker and invalidates all outstanding identities.
    pub fn clear(&mut self) {
        let ids = std::mem::take(&mut self.ids);
        self.markers.clear();
        for id in ids {
            self.release(id);
        }
        self.changed();
    }

    /// Counts marker starts in the renderer's widened overload window.
    #[must_use]
    pub fn overload_count(&self, tmin: f64, tmax: f64) -> usize {
        self.ensure_time_index();
        let index = self.time_index.borrow();
        let lower = tmin - index.max_span;
        let begin = index
            .starts
            .partition_point(|(time, _)| time.total_cmp(&lower).is_lt());
        let end = index
            .starts
            .partition_point(|(time, _)| !time.total_cmp(&tmax).is_gt());
        end.saturating_sub(begin)
    }

    /// Returns the markers whose endpoints fall inside a time tolerance.
    #[must_use]
    pub fn candidates_near(&self, time: f64, tolerance: f64, maximum: usize) -> MarkerCandidates {
        self.ensure_time_index();
        let index = self.time_index.borrow();
        let lower = time - tolerance.abs();
        let upper = time + tolerance.abs();
        let begin = index
            .endpoints
            .partition_point(|(endpoint, _)| endpoint.total_cmp(&lower).is_lt());
        let end = index
            .endpoints
            .partition_point(|(endpoint, _)| !endpoint.total_cmp(&upper).is_gt());
        let candidate_count = end.saturating_sub(begin);
        if candidate_count > maximum {
            return MarkerCandidates::Overloaded {
                count: candidate_count,
            };
        }

        let mut seen = HashSet::with_capacity(candidate_count);
        let mut ids = Vec::with_capacity(candidate_count);
        for &(_, id) in index.endpoints.get(begin..end).unwrap_or_default() {
            if seen.insert(id) {
                ids.push(id);
            }
        }
        ids.sort_by_key(|id| self.index_of(*id).unwrap_or(usize::MAX));
        MarkerCandidates::Exact(ids)
    }

    fn changed(&mut self) {
        self.store_generation = self.store_generation.wrapping_add(1);
    }

    fn release(&mut self, id: MarkerId) {
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
        for (id, marker) in self.iter_with_ids() {
            starts.push((marker.tmin, id));
            endpoints.push((marker.tmin, id));
            if marker.tmax > marker.tmin {
                endpoints.push((marker.tmax, id));
            }
            max_span = max_span.max(marker.tmax - marker.tmin);
        }
        starts.sort_by(|left, right| left.0.total_cmp(&right.0));
        endpoints.sort_by(|left, right| left.0.total_cmp(&right.0));
        *self.time_index.borrow_mut() = TimeIndex {
            store_generation: self.store_generation,
            starts,
            endpoints,
            max_span,
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
        self.changed();
        self.markers.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::{CommitOutcome, MarkerBudget, MarkerCandidates, MarkerStore, StagedMarkers};
    use crate::markers::Marker;

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
            CommitOutcome::Truncated {
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
}
