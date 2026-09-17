//! Segment identity, revisions, and the newest-revision-wins store
//! (spec 7.2 rule 4).
//!
//! Two halves of one rule. [`UtteranceTracker`] is the producer side: the
//! adapter mints one id per utterance and increments a revision for each
//! partial, so the invariant holds even for providers with no notion of a
//! revision. [`SegmentStore`] is the consumer side: it keeps only the newest
//! revision of each id and refuses to go backwards.

use std::collections::HashMap;

use crate::TranscriptSegment;

/// Source of segment ids.
///
/// Exists so golden tests can pin ids. Production uses [`UlidFactory`].
pub trait SegmentIdFactory {
    /// Mint the next id.
    fn next_id(&mut self) -> String;
}

/// The production id factory: monotonic ULIDs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UlidFactory;

impl SegmentIdFactory for UlidFactory {
    fn next_id(&mut self) -> String {
        crate::transcript::next_segment_id()
    }
}

/// A deterministic id factory: `prefix-0`, `prefix-1`, …
///
/// For golden files and fixtures, where a random ULID would make the expected
/// output unwritable. Not for production — the ids are not unique across
/// sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountingIdFactory {
    prefix: String,
    next: u64,
}

impl CountingIdFactory {
    /// A factory producing `prefix-0`, `prefix-1`, …
    #[must_use]
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            next: 0,
        }
    }
}

impl SegmentIdFactory for CountingIdFactory {
    fn next_id(&mut self) -> String {
        let id = format!("{}-{}", self.prefix, self.next);
        self.next += 1;
        id
    }
}

/// Assigns one id per utterance and a revision per emission.
///
/// The provider tells us *when* an utterance ends; the tracker decides what it
/// is called and what revision each emission carries. Keeping that decision on
/// our side is what lets rule 4 hold uniformly across providers whose streaming
/// protocols have nothing in common.
#[derive(Debug, Clone)]
pub struct UtteranceTracker<F = UlidFactory> {
    factory: F,
    /// The id of the utterance currently being revised, if one is open.
    open_id: Option<String>,
    /// The revision the next emission of the open utterance will carry.
    next_revision: u32,
}

impl Default for UtteranceTracker<UlidFactory> {
    fn default() -> Self {
        Self::new()
    }
}

impl UtteranceTracker<UlidFactory> {
    /// A tracker minting ULIDs.
    #[must_use]
    pub fn new() -> Self {
        Self::with_id_factory(UlidFactory)
    }
}

impl<F: SegmentIdFactory> UtteranceTracker<F> {
    /// A tracker minting ids from `factory`.
    pub fn with_id_factory(factory: F) -> Self {
        Self {
            factory,
            open_id: None,
            next_revision: 0,
        }
    }

    /// The id and revision for the next partial of the open utterance.
    ///
    /// Opens a new utterance if none is open.
    pub fn next_partial(&mut self) -> (String, u32) {
        let id = self.open_or_start();
        let revision = self.next_revision;
        self.next_revision += 1;
        (id, revision)
    }

    /// The id and revision for the final that closes the open utterance.
    ///
    /// Carries the same id as every partial before it, then closes the
    /// utterance so the next partial starts a fresh one at revision 0.
    pub fn next_final(&mut self) -> (String, u32) {
        let id = self.open_or_start();
        let revision = self.next_revision;
        self.open_id = None;
        self.next_revision = 0;
        (id, revision)
    }

    /// The open utterance's id, if one is open.
    #[must_use]
    pub fn open_id(&self) -> Option<&str> {
        self.open_id.as_deref()
    }

    /// Abandon the open utterance without emitting a final.
    ///
    /// Only correct where no final can ever arrive — a hard close, or a failover
    /// that abandons the provider mid-utterance. It leaves the last partial
    /// unsuperseded in the store, which is why it is explicit rather than
    /// something [`next_partial`](Self::next_partial) does implicitly.
    pub fn abandon(&mut self) {
        self.open_id = None;
        self.next_revision = 0;
    }

    fn open_or_start(&mut self) -> String {
        match &self.open_id {
            Some(id) => id.clone(),
            None => {
                let id = self.factory.next_id();
                self.open_id = Some(id.clone());
                self.next_revision = 0;
                id
            }
        }
    }
}

/// What a [`SegmentStore::upsert`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    /// The id was new.
    Inserted,
    /// A newer revision replaced the one that was there.
    Superseded {
        /// The revision that was replaced.
        previous_revision: u32,
    },
    /// The incoming revision was not newer, so it was discarded.
    RejectedStale {
        /// The revision that stayed.
        kept_revision: u32,
        /// The revision that was thrown away.
        rejected_revision: u32,
    },
}

impl StoreOutcome {
    /// Whether the store's contents changed.
    #[must_use]
    pub fn applied(&self) -> bool {
        !matches!(self, StoreOutcome::RejectedStale { .. })
    }
}

/// Holds the newest revision of every segment, in first-seen order.
///
/// Deliberately not a database: this is the in-memory view a stream feeds and
/// the UI reads. Persistence is `fotw-store`'s problem.
#[derive(Debug, Clone, Default)]
pub struct SegmentStore {
    by_id: HashMap<String, TranscriptSegment>,
    /// Ids in first-seen order. A revision never reorders the transcript, which
    /// is what makes the UI able to update a line in place.
    order: Vec<String>,
}

impl SegmentStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a segment, or replace an older revision of the same id.
    ///
    /// A revision that is not strictly newer is rejected rather than applied.
    /// Equal revisions are rejected too: after a reconnect the provider replays
    /// messages verbatim, so an equal revision carries no new information, and
    /// treating it as an update would let a replayed partial un-finalize a line
    /// that had already been finalized.
    pub fn upsert(&mut self, segment: TranscriptSegment) -> StoreOutcome {
        match self.by_id.get(&segment.id) {
            Some(existing) if segment.revision <= existing.revision => {
                StoreOutcome::RejectedStale {
                    kept_revision: existing.revision,
                    rejected_revision: segment.revision,
                }
            }
            Some(existing) => {
                let previous_revision = existing.revision;
                self.by_id.insert(segment.id.clone(), segment);
                StoreOutcome::Superseded { previous_revision }
            }
            None => {
                self.order.push(segment.id.clone());
                self.by_id.insert(segment.id.clone(), segment);
                StoreOutcome::Inserted
            }
        }
    }

    /// The newest revision of `id`, if present.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&TranscriptSegment> {
        self.by_id.get(id)
    }

    /// Every segment, newest revision only, in first-seen order.
    pub fn segments(&self) -> impl Iterator<Item = &TranscriptSegment> {
        self.order.iter().filter_map(|id| self.by_id.get(id))
    }

    /// Ids of segments that have not yet been finalized.
    ///
    /// The conformance suite (spec 7.3) asserts this is empty at end of session:
    /// every partial must eventually be superseded by a final.
    #[must_use]
    pub fn pending_ids(&self) -> Vec<String> {
        self.segments()
            .filter(|segment| !segment.is_final)
            .map(|segment| segment.id.clone())
            .collect()
    }

    /// How many distinct segments are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether the store is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}
