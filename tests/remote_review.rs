use murmur::remote_review::*;
fn window() -> Option<Window> {
    Some(Window {
        process: 42,
        element: 7,
    })
}
#[test]
fn opening_review_ui_retains_the_observed_destination_without_guessing_one() {
    let mut selection = Selection::default();
    let mut review = Review::default();
    selection.observe(Foreground::ReviewUi);
    assert_eq!(
        review.prepare("fixture", selection.take()),
        Err(Error::NoWindow)
    );
    selection.observe(Foreground::Destination(window().unwrap()));
    selection.observe(Foreground::ReviewUi);
    let id = review.prepare("fixture", selection.take()).unwrap();
    assert_eq!(review.confirm(id, window(), true), Ok("fixture".into()));
}
#[test]
fn unrelated_app_clears_destination_and_selecting_a_new_window_replaces_it() {
    let mut selection = Selection::default();
    let mut review = Review::default();
    selection.observe(Foreground::Destination(window().unwrap()));
    selection.observe(Foreground::Other);
    selection.observe(Foreground::ReviewUi);
    assert_eq!(
        review.prepare("fixture", selection.take()),
        Err(Error::NoWindow)
    );
    selection.observe(Foreground::Destination(window().unwrap()));
    let next = Window {
        process: 42,
        element: 8,
    };
    selection.observe(Foreground::Destination(next));
    selection.observe(Foreground::ReviewUi);
    let id = review.prepare("fixture", selection.take()).unwrap();
    assert_eq!(review.confirm(id, Some(next), true), Ok("fixture".into()));
}
#[test]
fn explicit_confirmation_releases_exact_reviewed_text_only_once() {
    let mut review = Review::default();
    let id = review.prepare("Café 👋", window()).unwrap();
    assert_eq!(
        review.confirm(id, window(), false),
        Err(Error::ConfirmationNeeded)
    );
    assert_eq!(review.confirm(id, window(), true), Ok("Café 👋".into()));
    assert_eq!(review.confirm(id, window(), true), Err(Error::Stale));
}
#[test]
fn changing_the_selected_window_invalidates_review() {
    for changed in [
        None,
        Some(Window {
            process: 43,
            element: 7,
        }),
        Some(Window {
            process: 42,
            element: 8,
        }),
    ] {
        let mut review = Review::default();
        let id = review.prepare("fixture", window()).unwrap();
        assert_eq!(review.confirm(id, changed, true), Err(Error::ChangedWindow));
        assert_eq!(review.confirm(id, window(), true), Err(Error::Stale));
    }
}
#[test]
fn new_review_and_cancellation_reject_old_actions() {
    let mut review = Review::default();
    let old = review.prepare("first", window()).unwrap();
    let new = review.prepare("second", window()).unwrap();
    assert_ne!(old, new);
    assert_eq!(review.confirm(old, window(), true), Err(Error::Stale));
    assert_eq!(review.confirm(new, window(), true), Ok("second".into()));
    let id = review.prepare("third", window()).unwrap();
    review.cancel();
    assert_eq!(review.confirm(id, window(), true), Err(Error::Stale));
}
#[test]
fn missing_window_and_invalid_text_never_create_shareable_review() {
    let mut review = Review::default();
    assert_eq!(review.prepare("fixture", None), Err(Error::NoWindow));
    for text in ["", "hi\n", "hi\t", "hi\u{2028}"] {
        assert_eq!(review.prepare(text, window()), Err(Error::InvalidText));
    }
}
