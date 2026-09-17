//! Spec 7.2, normalization rule 4.
//!
//! > Partials carry the same `id` as the final that supersedes them, with
//! > `revision` incremented. The store keeps only the newest revision.
//!
//! Sharing the id is what lets the UI replace a partial in place instead of
//! appending a near-duplicate line, and what lets the WAL be replayed without
//! deduplicating on text. The store's job is to make "newest revision wins"
//! true even when messages arrive out of order, which they do: a reconnect can
//! deliver a replayed older revision after a newer one has already landed.

use fotw_stt::{
    CountingIdFactory, SegmentStore, Source, StoreOutcome, TranscriptSegment, UtteranceTracker,
};

fn segment(id: &str, revision: u32, text: &str, is_final: bool) -> TranscriptSegment {
    let mut segment = TranscriptSegment::new("session", Source::System, "deepgram", "nova-3");
    segment.id = id.to_string();
    segment.revision = revision;
    segment.text = text.to_string();
    segment.is_final = is_final;
    segment
}

#[test]
fn a_partial_is_replaced_in_place_by_its_final() {
    let mut store = SegmentStore::new();

    assert_eq!(
        store.upsert(segment("A", 0, "we should", false)),
        StoreOutcome::Inserted
    );
    assert_eq!(
        store.upsert(segment("A", 1, "we should ship", false)),
        StoreOutcome::Superseded {
            previous_revision: 0
        }
    );
    assert_eq!(
        store.upsert(segment("A", 2, "We should ship on Friday.", true)),
        StoreOutcome::Superseded {
            previous_revision: 1
        }
    );

    // One line in the transcript, not three.
    assert_eq!(store.len(), 1);
    let kept = store.get("A").expect("segment A is present");
    assert_eq!(kept.text, "We should ship on Friday.");
    assert_eq!(kept.revision, 2);
    assert!(kept.is_final);
}

#[test]
fn an_out_of_order_older_revision_is_rejected_not_applied() {
    // The case this store exists for. After a reconnect, STT-09 replays PCM, and
    // the provider can re-emit an earlier partial for audio we already finalized.
    // Applying it would visibly un-finalize a line and truncate its text.
    let mut store = SegmentStore::new();

    store.upsert(segment("A", 0, "we should", false));
    store.upsert(segment("A", 3, "We should ship on Friday.", true));

    let outcome = store.upsert(segment("A", 1, "we should ship", false));

    assert_eq!(
        outcome,
        StoreOutcome::RejectedStale {
            kept_revision: 3,
            rejected_revision: 1
        }
    );

    let kept = store.get("A").expect("segment A is present");
    assert_eq!(
        kept.text, "We should ship on Friday.",
        "a stale revision overwrote a newer one"
    );
    assert_eq!(kept.revision, 3);
    assert!(kept.is_final, "a stale partial un-finalized a final");
    assert_eq!(store.len(), 1);
}

#[test]
fn a_repeat_of_the_current_revision_is_rejected_as_a_duplicate() {
    // Replay after a reconnect re-delivers messages verbatim. Same revision
    // means same content, so there is nothing to apply.
    let mut store = SegmentStore::new();
    store.upsert(segment("A", 2, "We should ship on Friday.", true));

    let outcome = store.upsert(segment("A", 2, "We should ship on Friday.", true));

    assert_eq!(
        outcome,
        StoreOutcome::RejectedStale {
            kept_revision: 2,
            rejected_revision: 2
        }
    );
    assert_eq!(store.len(), 1);
}

#[test]
fn distinct_utterances_are_kept_in_first_seen_order() {
    let mut store = SegmentStore::new();

    store.upsert(segment("A", 0, "first", false));
    store.upsert(segment("B", 0, "second", false));
    store.upsert(segment("A", 1, "first, revised", true));
    store.upsert(segment("C", 0, "third", false));

    assert_eq!(store.len(), 3);
    let texts: Vec<&str> = store.segments().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["first, revised", "second", "third"]);
}

#[test]
fn the_store_reports_which_utterances_are_still_open() {
    // The conformance property from spec 7.3: every partial is eventually
    // superseded by a final. At end of session nothing may still be open.
    let mut store = SegmentStore::new();
    store.upsert(segment("A", 0, "still going", false));
    store.upsert(segment("B", 0, "done", true));

    assert_eq!(store.pending_ids(), vec!["A".to_string()]);

    store.upsert(segment("A", 1, "still going, done", true));
    assert!(store.pending_ids().is_empty());
}

#[test]
fn the_utterance_tracker_keeps_one_id_across_partials_and_increments_revision() {
    // This is the producer side of rule 4: the adapter, not the provider,
    // assigns ids and revisions, which is what guarantees the invariant holds
    // for every provider including the ones with no notion of a revision.
    let mut tracker = UtteranceTracker::with_id_factory(CountingIdFactory::new("seg"));

    let (first_id, first_revision) = tracker.next_partial();
    let (second_id, second_revision) = tracker.next_partial();
    let (final_id, final_revision) = tracker.next_final();

    assert_eq!(first_id, "seg-0");
    assert_eq!(second_id, "seg-0", "a partial must not mint a new id");
    assert_eq!(final_id, "seg-0", "the final must carry the partials' id");
    assert_eq!((first_revision, second_revision, final_revision), (0, 1, 2));

    // Finalizing closes the utterance; the next partial starts a new one at
    // revision 0.
    let (next_id, next_revision) = tracker.next_partial();
    assert_eq!(next_id, "seg-1");
    assert_eq!(next_revision, 0);
}

#[test]
fn tracked_segments_land_in_the_store_as_a_single_line() {
    let mut tracker = UtteranceTracker::with_id_factory(CountingIdFactory::new("seg"));
    let mut store = SegmentStore::new();

    for text in ["we", "we should", "we should ship"] {
        let (id, revision) = tracker.next_partial();
        store.upsert(segment(&id, revision, text, false));
    }
    let (id, revision) = tracker.next_final();
    store.upsert(segment(&id, revision, "We should ship.", true));

    assert_eq!(store.len(), 1);
    assert_eq!(store.get("seg-0").unwrap().text, "We should ship.");
    assert_eq!(store.get("seg-0").unwrap().revision, 3);
    assert!(store.pending_ids().is_empty());
}

#[test]
fn a_tracker_with_the_default_factory_mints_ulids() {
    let mut tracker = UtteranceTracker::new();
    let (id, revision) = tracker.next_final();

    assert_eq!(id.len(), 26, "default ids are ULIDs: {id}");
    assert_eq!(revision, 0, "a final with no preceding partial starts at 0");
}
