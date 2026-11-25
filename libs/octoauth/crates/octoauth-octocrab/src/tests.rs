//! The adapter against a local GitHub stand-in (wiremock), with a real
//! octocrab client and octoauth `App`.

use super::*;
use octoauth::{ForbiddenReason, SigningKey, WithCredentialError};
use std::time::{SystemTime, UNIX_EPOCH};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PEM: &str = include_str!("../tests/fixtures/rsa2048.pem");

fn app(server: &MockServer) -> App {
    App::builder(7, SigningKey::from_pem(PEM).unwrap())
        .base_url(server.uri())
        .build()
        .unwrap()
}

fn base(server: &MockServer) -> Octocrab {
    builder(&app(server)).unwrap().build().unwrap()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

/// An HTTP `Date` header value for Unix time `seconds`.
fn http_date(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{weekday}, {day:02} {} {year} {:02}:{:02}:{:02} GMT",
        months[(month - 1) as usize],
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

fn installations_body() -> serde_json::Value {
    serde_json::json!([])
}

/// `GET /app/installations` through [`App::with_credential`].
async fn list_installations(
    app: &App,
    base: &Octocrab,
) -> Result<usize, WithCredentialError<octocrab::Error>> {
    app.with_credential(unauthorized, |credential| async move {
        let github = authenticated(base, credential.expose_secret())?;
        Ok(github.apps().installations().send().await?.items.len())
    })
    .await
}

#[test]
fn http_dates_are_rfc_7231() {
    assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    assert_eq!(http_date(1_800_000_000), "Fri, 15 Jan 2027 08:00:00 GMT");
}

#[tokio::test]
async fn the_builder_targets_the_apps_api_without_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(installations_body()))
        .mount(&server)
        .await;
    let base = base(&server);

    // The trailing `/` octoauth adds to the base does not double the slash.
    assert_eq!(list_installations(&app(&server), &base).await.unwrap(), 0);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/app/installations");
    let authorization: Vec<_> = requests[0]
        .headers
        .get_all("authorization")
        .iter()
        .collect();
    assert_eq!(authorization.len(), 1, "exactly one Authorization header");
    assert!(authorization[0].to_str().unwrap().starts_with("Bearer eyJ"));

    // The base client itself sends none.
    let _ = base.apps().installations().send().await;
    let requests = server.received_requests().await.unwrap();
    assert!(!requests[1].headers.contains_key("authorization"));
}

#[tokio::test]
async fn a_skewed_clock_is_corrected_by_one_retry() {
    // GitHub's clock 660 s ahead: the first credential has expired for it.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("date", http_date(now() + 660).as_str())
                .set_body_json(serde_json::json!({"message": "'Expiration time' claim ('exp') is too far in the past"})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(installations_body()))
        .mount(&server)
        .await;
    let app = app(&server);

    assert_eq!(list_installations(&app, &base(&server)).await.unwrap(), 0);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_ne!(
        requests[0].headers["authorization"], requests[1].headers["authorization"],
        "re-signed with the correction"
    );
}

#[tokio::test]
async fn a_rejection_the_clock_does_not_explain_is_returned_without_a_retry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("date", http_date(now()).as_str())
                .set_body_json(
                    serde_json::json!({"message": "A JSON web token could not be decoded"}),
                ),
        )
        .mount(&server)
        .await;

    let error = list_installations(&app(&server), &base(&server))
        .await
        .unwrap_err();
    let WithCredentialError::Call(error) = error else {
        panic!("expected the call's error, got {error:?}");
    };
    assert!(is_unauthorized(&error));
    assert!(unauthorized(&error).is_some_and(|headers| headers.contains_key("date")));
    assert_eq!(Error::from(error), Error::Api(ApiError::Unauthorized));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn errors_are_classified_without_their_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/installations"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "message": "Although you appear to have the correct authorization credentials, the `acme` organization has an IP allow list enabled, and your IP address is not permitted to access this resource."
        })))
        .mount(&server)
        .await;

    let error = list_installations(&app(&server), &base(&server))
        .await
        .unwrap_err();
    let WithCredentialError::Call(error) = error else {
        panic!("expected the call's error, got {error:?}");
    };
    assert!(!is_unauthorized(&error));
    assert!(unauthorized(&error).is_none());
    let error = Error::from(error);
    assert_eq!(
        error,
        Error::Api(ApiError::Forbidden {
            reason: ForbiddenReason::IpAllowList
        })
    );
    assert_eq!(error.code(), "gh:api:forbidden");
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains("acme"), "{rendered}");
}

#[tokio::test]
async fn no_answer_is_a_client_error_without_details() {
    // A port nothing listens on. (A dropped `MockServer` would not do:
    // wiremock pools its servers, so the port keeps answering.)
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let app = App::builder(7, SigningKey::from_pem(PEM).unwrap())
        .base_url(format!("http://127.0.0.1:{port}"))
        .build()
        .unwrap();
    let base = builder(&app).unwrap().build().unwrap();

    let error = list_installations(&app, &base).await.unwrap_err();
    let WithCredentialError::Call(error) = error else {
        panic!("expected the call's error, got {error:?}");
    };
    assert!(unauthorized(&error).is_none());
    let error = Error::from(error);
    assert_eq!(error, Error::Client);
    assert_eq!(error.code(), "gh:octocrab:client");
    assert_eq!(error.to_string(), "gh:octocrab:client");
}
