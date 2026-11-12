//! Channel-scope matching and generation-cached per-track membership.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::{Marker, MarkerId};
use crate::bridge::ViewTrack;

/// Cached membership for one row, sharing the store's unscoped marker set.
pub struct TrackMarkers {
    unscoped: Rc<HashSet<MarkerId>>,
    scoped: HashSet<MarkerId>,
}

impl TrackMarkers {
    /// Reports whether a marker applies to this row or concrete trace.
    #[must_use]
    pub fn contains(&self, id: MarkerId) -> bool {
        self.unscoped.contains(&id) || self.scoped.contains(&id)
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ChannelKey {
    net: String,
    sta: String,
    loc: String,
    cha: String,
    grouped: bool,
}

#[derive(Default)]
struct StationMarkers {
    channels: HashMap<(String, String), Vec<MarkerId>>,
}

#[derive(Default)]
pub(super) struct ScopeIndex {
    pub(super) generation: u64,
    unscoped: Rc<HashSet<MarkerId>>,
    exact: HashMap<(String, String), StationMarkers>,
    patterns: HashMap<[String; 4], Vec<MarkerId>>,
    rows: HashMap<ChannelKey, Rc<TrackMarkers>>,
}

impl ScopeIndex {
    pub(super) fn build<'a>(
        generation: u64,
        markers: impl Iterator<Item = (MarkerId, &'a Marker)>,
    ) -> Self {
        let mut index = Self {
            generation,
            ..Self::default()
        };
        let mut unscoped = HashSet::new();
        for (id, marker) in markers {
            if marker.nslc_ids.is_empty() {
                unscoped.insert(id);
            }
            for (net, sta, loc, cha) in &marker.nslc_ids {
                let codes = [net.clone(), sta.clone(), loc.clone(), cha.clone()];
                if codes.iter().any(|code| code.contains(['*', '?', '['])) {
                    index.patterns.entry(codes).or_default().push(id);
                } else {
                    index
                        .exact
                        .entry((net.clone(), sta.clone()))
                        .or_default()
                        .channels
                        .entry((loc.clone(), cha.clone()))
                        .or_default()
                        .push(id);
                }
            }
        }
        index.unscoped = Rc::new(unscoped);
        index
    }

    pub(super) fn matching(
        &mut self,
        track: &ViewTrack,
        loc: &str,
        grouped: bool,
    ) -> Rc<TrackMarkers> {
        let key = ChannelKey {
            net: track.net.clone(),
            sta: track.sta.clone(),
            loc: loc.to_owned(),
            cha: track.cha.clone(),
            grouped,
        };
        if let Some(ids) = self.rows.get(&key) {
            return Rc::clone(ids);
        }
        let mut scoped = HashSet::new();
        let component =
            |pattern: &str, code: &str| (grouped && code == "*") || glob_match(pattern, code);
        let mut station_matches = |station: &StationMarkers| {
            if !grouped || (loc != "*" && track.cha != "*") {
                if let Some(ids) = station.channels.get(&(loc.to_owned(), track.cha.clone())) {
                    scoped.extend(ids);
                }
            } else {
                for ((location, channel), ids) in &station.channels {
                    if (loc == "*" || loc == location)
                        && (track.cha == "*" || track.cha == *channel)
                    {
                        scoped.extend(ids);
                    }
                }
            }
        };
        if grouped && (track.net == "*" || track.sta == "*") {
            for ((net, sta), station) in &self.exact {
                if (track.net == "*" || track.net == *net)
                    && (track.sta == "*" || track.sta == *sta)
                {
                    station_matches(station);
                }
            }
        } else if let Some(station) = self.exact.get(&(track.net.clone(), track.sta.clone())) {
            station_matches(station);
        }
        for ([net, sta, location, cha], ids) in &self.patterns {
            if component(net, &track.net)
                && component(sta, &track.sta)
                && component(location, loc)
                && component(cha, &track.cha)
            {
                scoped.extend(ids);
            }
        }
        let members = Rc::new(TrackMarkers {
            unscoped: Rc::clone(&self.unscoped),
            scoped,
        });
        self.rows.insert(key, Rc::clone(&members));
        members
    }
}

/// Shell-style glob match (`*`, `?`, `[seq]`, `[!seq]`), anchored at both ends.
///
/// This is the dialect `util.match_nslc` builds from `fnmatch.translate`, which is
/// what snuffler matches a marker's channel patterns with (`marker.py:220`).
/// Patterns are a handful of characters, so the backtracking `*` is harmless.
fn glob_match(pattern: &str, value: &str) -> bool {
    fn matches(pattern: &[char], value: &[char]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some(('*', rest)) => {
                (0..=value.len()).any(|skip| matches(rest, value.get(skip..).unwrap_or(&[])))
            }
            Some(('?', rest)) => value
                .split_first()
                .is_some_and(|(_, value_rest)| matches(rest, value_rest)),
            Some(('[', rest)) => {
                let Some(end) = rest.iter().position(|&c| c == ']') else {
                    // An unterminated class matches a literal '[', as fnmatch's does.
                    return value.split_first().is_some_and(|(&first, value_rest)| {
                        first == '[' && matches(rest, value_rest)
                    });
                };
                let (class, tail) = rest.split_at(end);
                let tail = tail.get(1..).unwrap_or(&[]);
                let (negated, class) = match class.split_first() {
                    Some(('!', class)) => (true, class),
                    _ => (false, class),
                };
                let Some((&candidate, value_rest)) = value.split_first() else {
                    return false;
                };
                class_contains(class, candidate) != negated && matches(tail, value_rest)
            }
            Some((&literal, rest)) => value
                .split_first()
                .is_some_and(|(&first, value_rest)| first == literal && matches(rest, value_rest)),
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    matches(&pattern, &value)
}

/// Whether a glob character class contains `candidate`.
///
/// The class is walked as `low-high` ranges and single characters; a dash without a
/// following bound is a literal, as in fnmatch.
fn class_contains(class: &[char], candidate: char) -> bool {
    let mut contained = false;
    let mut cursor = class;
    while let Some((&first, rest)) = cursor.split_first() {
        if let Some(('-', after_dash)) = rest.split_first()
            && let Some((&high, after_range)) = after_dash.split_first()
        {
            contained |= first <= candidate && candidate <= high;
            cursor = after_range;
        } else {
            contained |= first == candidate;
            cursor = rest;
        }
    }
    contained
}

/// Whether an NSLC-scoped marker belongs on a row.
///
/// This is per-component fnmatch, exactly `util.match_nslc` over a pattern tuple: a
/// partial wildcard like `BH?` or `6*` matches, and an empty component matches only
/// an empty code.
/// Qt's Cake Phase writes `('*', '*')` for "any location, any channel", and `''`
/// means "the blank location".
/// A track that groups over a component reports `'*'` for it and accepts any
/// pattern there, since the row holds every value.
/// Matching the identity rather than the spans keeps a marker on its row even when
/// the marker's own location has no data in the window.
#[must_use]
pub fn marker_matches_track(nslc: &(String, String, String, String), track: &ViewTrack) -> bool {
    let (net, sta, loc, cha) = nslc;
    let component = |pattern: &str, code: &str| code == "*" || glob_match(pattern, code);
    component(net, &track.net)
        && component(sta, &track.sta)
        && component(loc, &track.loc)
        && component(cha, &track.cha)
}
