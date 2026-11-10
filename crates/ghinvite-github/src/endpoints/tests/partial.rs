use super::*;
use crate::{error::PartialReason, rest::TransportError};

fn path() -> PathTemplate {
    REPO.path(Endpoint::ListRepoInvitations)
}
fn page(number: usize, sso: Option<&str>, next: bool) -> Expectation {
    let url = if number == 1 {
        "https://api.github.test/repos/acme/api/invitations?per_page=100".to_owned()
    } else {
        format!("https://api.github.test/repos/acme/api/invitations?page={number}")
    };
    let mut page = Expectation::ok_json(Method::Get, url, json!([number]))
        .require_header("x-github-api-version", "2022-11-28");
    if let Some(sso) = sso {
        page.response
            .headers
            .insert("x-github-sso".into(), sso.into());
    }
    if next {
        page.response.headers.insert(
            "link".into(),
            format!(
                "<https://api.github.test/repos/acme/api/invitations?page={}>; rel=next",
                number + 1
            ),
        );
    }
    page
}

#[tokio::test]
// covers: PG-07
async fn explicit_partial_collect_and_stream_keep_items_and_merge_bounded_numeric_evidence() {
    for streaming in [false, true] {
        let first = format!(
            "partial-results; organizations=0,PRIVATE_HEADER_MARKER,{}",
            (1..=90)
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let second = format!(
            "partial-results; organizations={}",
            (80..=130)
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let mock = MockTransport::scripted(vec![
            page(1, Some(&first), true),
            page(2, Some(&second), true),
            page(3, None, false),
        ]);
        let env = env(&mock);
        let cred = installation();
        let rest = Rest::new(&env, &cred);
        let (items, partial) = if streaming {
            let mut seen = Vec::new();
            let completion = rest
                .allow_partial()
                .for_each_page::<usize>(Endpoint::ListRepoInvitations, path(), &[], |items| {
                    seen.extend(items);
                    Ok(())
                })
                .await
                .unwrap();
            assert!(!completion.complete());
            (seen, completion.partial)
        } else {
            let listing = rest
                .allow_partial()
                .list_all::<usize>(Endpoint::ListRepoInvitations, path(), &[])
                .await
                .unwrap();
            assert!(!listing.complete());
            (listing.items, listing.partial)
        };
        assert_eq!(items, vec![1, 2, 3]);
        let partial = partial.unwrap();
        assert_eq!(partial.reason, PartialReason::SsoRequired);
        assert_eq!(
            partial
                .org_ids
                .iter()
                .map(|id| id.get())
                .collect::<Vec<_>>(),
            (1..=100).collect::<Vec<_>>()
        );
        assert!(!format!("{partial:?}").contains("PRIVATE_HEADER_MARKER"));
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn default_partial_paths_error_without_absorbing_omitted_page_or_fetching_next() {
    for streaming in [false, true] {
        let mock = MockTransport::scripted(vec![
            page(1, None, true),
            page(2, Some("partial-results; organizations=12,secret,34"), true),
        ]);
        let env = env(&mock);
        let cred = installation();
        let rest = Rest::new(&env, &cred);
        let mut seen = Vec::new();
        let error = if streaming {
            rest.for_each_page::<usize>(Endpoint::ListRepoInvitations, path(), &[], |items| {
                seen.extend(items);
                Ok(())
            })
            .await
            .unwrap_err()
        } else {
            rest.list_all::<usize>(Endpoint::ListRepoInvitations, path(), &[])
                .await
                .unwrap_err()
        };
        assert_eq!(seen, if streaming { vec![1] } else { vec![] });
        assert!(
            matches!(error,ApiError::Partial{reason:PartialReason::SsoRequired,org_ids} if org_ids.iter().map(|id|id.get()).collect::<Vec<_>>()==vec![12,34])
        );
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn opt_in_complete_walk_is_distinct_from_partial_with_missing_org_ids() {
    for header in [None, Some("partial-results; organizations=private,0,-1")] {
        for streaming in [false, true] {
            let mock = MockTransport::scripted(vec![page(1, header, false)]);
            let env = env(&mock);
            let cred = installation();
            let rest = Rest::new(&env, &cred);
            let partial = if streaming {
                let result = rest
                    .allow_partial()
                    .for_each_page::<usize>(Endpoint::ListRepoInvitations, path(), &[], |items| {
                        assert_eq!(items, vec![1]);
                        Ok(())
                    })
                    .await
                    .unwrap();
                assert_eq!(result.complete(), header.is_none());
                result.partial
            } else {
                let result = rest
                    .allow_partial()
                    .list_all::<usize>(Endpoint::ListRepoInvitations, path(), &[])
                    .await
                    .unwrap();
                assert_eq!(result.items, vec![1]);
                assert_eq!(result.complete(), header.is_none());
                result.partial
            };
            assert_eq!(partial.is_some(), header.is_some());
            if let Some(info) = partial {
                assert!(info.org_ids.is_empty());
            }
            mock.assert_exhausted();
        }
    }
}

#[tokio::test]
// covers: PG-01 PG-07
async fn partial_opt_in_never_turns_later_http_decode_or_pagination_failure_into_success() {
    for streaming in [false, true] {
        for case in [
            "http",
            "decode",
            "foreign",
            "endpoint",
            "cycle",
            "fragment",
            "malformed_link",
        ] {
            let first = page(1, Some("partial-results; organizations=12"), true);
            let mut second = page(2, None, false);
            match case {
                "http" => second.response.status = 503,
                "decode" => second.response.body = b"PRIVATE_BODY_MARKER".to_vec(),
                "foreign" => {
                    second.response.headers.insert(
                        "link".into(),
                        "<https://evil.invalid/private>; rel=next".into(),
                    );
                }
                "endpoint" => {
                    second.response.headers.insert(
                        "link".into(),
                        "<https://api.github.test/user/emails>; rel=next".into(),
                    );
                }
                "cycle" => {
                    second.response.headers.insert("link".into(),"<https://api.github.test/repos/acme/api/invitations?per_page=100>; rel=next".into());
                }
                "fragment" => {
                    second.response.headers.insert("link".into(),"<https://api.github.test/repos/acme/api/invitations?page=3#private>; rel=next".into());
                }
                "malformed_link" => {
                    second
                        .response
                        .headers
                        .insert("link".into(), "PRIVATE_LINK_MARKER".into());
                }
                _ => unreachable!(),
            }
            let mock = MockTransport::scripted(vec![first, second]);
            let env = env(&mock);
            let cred = installation();
            let rest = Rest::new(&env, &cred);
            let mut seen = Vec::new();
            let error = if streaming {
                rest.allow_partial()
                    .for_each_page::<usize>(Endpoint::ListRepoInvitations, path(), &[], |items| {
                        seen.extend(items);
                        Ok(())
                    })
                    .await
                    .unwrap_err()
            } else {
                rest.allow_partial()
                    .list_all::<usize>(Endpoint::ListRepoInvitations, path(), &[])
                    .await
                    .unwrap_err()
            };
            if streaming {
                assert_eq!(seen[0], 1, "{case}");
            }
            assert!(
                !matches!(error, ApiError::Partial { .. }),
                "{case}: {error:?}"
            );
            let diagnostic = format!("{error:?} {error}");
            assert!(!diagnostic.contains("PRIVATE_"));
            mock.assert_exhausted();
        }
    }
}

struct LateTransportFailure {
    first: MockTransport,
    calls: std::sync::atomic::AtomicUsize,
    failure: TransportError,
}
impl crate::transport::HttpTransport for LateTransportFailure {
    async fn send(
        &self,
        request: crate::transport::Request,
    ) -> Result<crate::transport::Response, TransportError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            crate::transport::HttpTransport::send(&self.first, request).await
        } else {
            Err(self.failure)
        }
    }
}
#[tokio::test]
async fn opt_in_preserves_late_transport_and_callback_errors_and_send_future() {
    for failure in [
        TransportError::NotSent,
        TransportError::SentNoResponse { timed_out: true },
        TransportError::Oversize,
    ] {
        let mock = MockTransport::scripted(vec![page(
            1,
            Some("partial-results; organizations=12"),
            true,
        )]);
        let mut env = env(&mock);
        let transport = Arc::new(LateTransportFailure {
            first: mock.clone(),
            calls: 0.into(),
            failure,
        });
        env.transport = transport.clone();
        let cred = installation();
        let rest = Rest::new(&env, &cred);
        let partial = rest.allow_partial();
        let future = partial.list_all::<usize>(Endpoint::ListRepoInvitations, path(), &[]);
        fn assert_send(_: &impl Send) {}
        assert_send(&future);
        assert_eq!(future.await.unwrap_err(), ApiError::from(failure));
        assert_eq!(transport.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        mock.assert_exhausted();
    }
    let mock = MockTransport::scripted(vec![page(
        1,
        Some("partial-results; organizations=12"),
        true,
    )]);
    let env = env(&mock);
    let cred = installation();
    let rest = Rest::new(&env, &cred);
    let error = rest
        .allow_partial()
        .for_each_page::<usize>(Endpoint::ListRepoInvitations, path(), &[], |_| {
            Err(ApiError::Decode { what: "callback" })
        })
        .await
        .unwrap_err();
    assert_eq!(error, ApiError::Decode { what: "callback" });
    mock.assert_exhausted();
}

#[tokio::test]
async fn opt_in_keeps_credential_endpoint_and_thousand_page_guards() {
    let mock = MockTransport::scripted(vec![]);
    let env = env(&mock);
    let cred = app();
    let rest = Rest::new(&env, &cred);
    assert!(matches!(
        rest.allow_partial()
            .list_all::<usize>(Endpoint::ListRepoInvitations, path(), &[])
            .await,
        Err(ApiError::Forbidden {
            reason: ForbiddenReason::WrongCredential
        })
    ));
    let cred = installation();
    let rest = Rest::new(&env, &cred);
    assert_eq!(
        rest.allow_partial()
            .list_all::<usize>(Endpoint::PutCollaborator, path(), &[])
            .await
            .unwrap_err(),
        ApiError::Decode {
            what: "pagination_endpoint"
        }
    );
    mock.assert_exhausted();
    let mock = MockTransport::scripted(
        (1..=crate::pagination::MAX_PAGES)
            .map(|n| page(n, Some("partial-results; organizations=12"), true))
            .collect(),
    );
    let env = super::env(&mock);
    let rest = Rest::new(&env, &cred);
    assert_eq!(
        rest.allow_partial()
            .list_all::<usize>(Endpoint::ListRepoInvitations, path(), &[])
            .await
            .unwrap_err(),
        ApiError::Decode { what: "pagination" }
    );
    mock.assert_exhausted();
}

#[tokio::test]
// covers: PG-07
async fn generic_single_call_preserves_bounded_numeric_sso_evidence_without_decoding_data() {
    for header in [
        "partial-results; organizations=12,0,PRIVATE_SSO_MARKER,34,12",
        "PARTIAL-RESULTS; Organizations=12,0,PRIVATE_SSO_MARKER,34,12",
    ] {
        let mut response = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations",
            json!({"private": "PRIVATE_RESPONSE_MARKER"}),
        );
        response
            .response
            .headers
            .insert("x-github-sso".into(), header.into());
        let mock = MockTransport::scripted(vec![response]);
        let environment = env(&mock);
        let credential = installation();
        let error = Rest::new(&environment, &credential)
            .call::<(), Vec<u64>>(Method::Get, path(), &[], None)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ApiError::Partial {
                reason: PartialReason::SsoRequired,
                org_ids: vec![crate::ids::OrgId::new(12), crate::ids::OrgId::new(34)],
            }
        );
        let diagnostics = format!("{error} {error:?}");
        assert!(!diagnostics.contains("PRIVATE_SSO_MARKER"));
        assert!(!diagnostics.contains("PRIVATE_RESPONSE_MARKER"));
        mock.assert_exhausted();
    }
}
