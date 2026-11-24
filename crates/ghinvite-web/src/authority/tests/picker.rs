//! The create form's repository picker over a large installation listing.
use super::*;

/// The installation's `total` repositories `acme/r1…`, as GitHub lists them:
/// 100 to a page, each page linking the next.
fn listing(total: u64) -> Vec<Expectation> {
    let ids: Vec<u64> = (1..=total).collect();
    let pages = ids.chunks(100).count();
    ids.chunks(100)
        .enumerate()
        .map(|(index, chunk)| {
            let mut expectation = page(chunk);
            if index > 0 {
                expectation.url = format!(
                    "https://api.github.com/installation/repositories?per_page=100&page={}",
                    index + 1
                );
            }
            let mut body: serde_json::Value =
                serde_json::from_slice(&expectation.response.body).unwrap();
            body["total_count"] = json!(total);
            expectation.response.body = serde_json::to_vec(&body).unwrap();
            if index + 1 < pages {
                expectation.response.headers.insert(
                    "link".into(),
                    format!(
                        "<https://api.github.com/installation/repositories?per_page=100&page={}>; rel=\"next\"",
                        index + 2
                    ),
                );
            }
            expectation
        })
        .collect()
}
async fn owner(state: &AppState) -> AuthorityView {
    let view = AuthorityView::open(state, 9, "installer", &account())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.role(), Role::InstallingOwner);
    view
}
fn ids(repos: &[RepoTarget]) -> Vec<u64> {
    repos.iter().map(|repo| repo.id).collect()
}

#[tokio::test]
async fn an_unfiltered_page_reads_only_the_listing_pages_it_needs() {
    let (state, _, mock, limit) = fixture(listing(250), false).await;
    let view = owner(&state).await;

    // The form's first page: one listing read, however large the installation.
    let first = view.repository_picker("", 1, &[]).await.unwrap();
    assert_eq!(ids(&first.listed), (1..=50).collect::<Vec<_>>());
    assert_eq!((first.page, first.pages, first.matches), (1, 5, 250));
    assert!(first.selected.is_empty() && !first.pending);
    assert_eq!(limit.calls.load(Ordering::SeqCst), 1);
    assert_eq!(mock.remaining(), 2);

    // Page 3 (repositories 101–150) needs GitHub's second page, not its third.
    let third = view.repository_picker("", 3, &[]).await.unwrap();
    assert_eq!(ids(&third.listed), (101..=150).collect::<Vec<_>>());
    assert_eq!(limit.calls.load(Ordering::SeqCst), 2);
    assert_eq!(mock.remaining(), 1);

    // A selection inside the prefix costs nothing more.
    let selected = view.repository_picker("", 1, &[90, 3]).await.unwrap();
    assert_eq!(ids(&selected.selected), vec![3, 90]);
    assert_eq!(limit.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn searching_or_a_selection_beyond_the_prefix_reads_the_cached_listing_once() {
    let (state, _, mock, limit) = fixture(listing(250), false).await;
    let view = owner(&state).await;

    // A selection from GitHub's third page is kept on the first picker page.
    let first = view.repository_picker("", 1, &[230, 999]).await.unwrap();
    assert_eq!(ids(&first.listed), (1..=50).collect::<Vec<_>>());
    assert_eq!(ids(&first.selected), vec![230], "unknown ids are dropped");
    assert_eq!(first.matches, 250);
    assert_eq!(limit.calls.load(Ordering::SeqCst), 3);
    mock.assert_exhausted();

    // Searches and their pages filter the listing already read.
    let search = view.repository_picker(" R2 ", 1, &[]).await.unwrap();
    // r2, r20–r29, r200–r250.
    assert_eq!(search.matches, 1 + 10 + 51);
    assert_eq!(search.pages, 2);
    assert_eq!(ids(&search.listed)[..3], [2, 20, 21]);
    let second = view.repository_picker("r2", 2, &[]).await.unwrap();
    assert_eq!(second.listed.len(), 12);
    assert_eq!(second.page, 2);
    // A page past the end is the last one.
    let clamped = view.repository_picker("r2", 99, &[]).await.unwrap();
    assert_eq!((clamped.page, clamped.listed), (2, second.listed));
    let none = view.repository_picker("nothing", 1, &[]).await.unwrap();
    assert_eq!((none.matches, none.pages, none.listed.len()), (0, 1, 0));
    assert_eq!(limit.calls.load(Ordering::SeqCst), 3);

    // Another viewer within five minutes reads nothing from GitHub.
    let again = owner(&state).await;
    let page = again.repository_picker("", 5, &[250]).await.unwrap();
    assert_eq!(ids(&page.listed), (201..=250).collect::<Vec<_>>());
    assert_eq!(limit.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_delegated_picker_searches_first_and_lists_at_most_twenty_matches() {
    // Opening the delegated view reads GitHub's first page (the cardinality
    // check); the rest of the listing is read only when a search needs it.
    let (state, _, mock, limit) = fixture(listing(150), true).await;
    let view = delegate(&state).await;
    assert_eq!(limit.calls.load(Ordering::SeqCst), 1);

    let prompt = view.repository_picker("", 4, &[]).await.unwrap();
    assert!(prompt.search_required);
    assert!(prompt.listed.is_empty() && prompt.selected.is_empty());
    assert_eq!(limit.calls.load(Ordering::SeqCst), 1);

    let search = view.repository_picker("r1", 3, &[]).await.unwrap();
    assert_eq!(search.listed.len(), DELEGATED_SEARCH_LIMIT);
    assert!(search.truncated);
    assert_eq!((search.page, search.pages), (1, 1), "no paging");
    assert_eq!(search.matches, DELEGATED_SEARCH_LIMIT);
    assert_eq!(limit.calls.load(Ordering::SeqCst), 2);
    mock.assert_exhausted();

    let narrow = view.repository_picker("r14", 1, &[]).await.unwrap();
    // r14, r140–r149.
    assert_eq!(narrow.listed.len(), 11);
    assert!(!narrow.truncated);

    // Selections stay listed without a query, and nothing else is.
    let kept = view.repository_picker("", 1, &[150]).await.unwrap();
    assert!(kept.search_required);
    assert!(kept.listed.is_empty());
    assert_eq!(ids(&kept.selected), vec![150]);
    // No permission was read for anything listed.
    assert_eq!(limit.calls.load(Ordering::SeqCst), 2);
}
