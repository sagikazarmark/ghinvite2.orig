use crate::error::WebError;
use crate::views::render::render;
use axum::body::Body;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use dioxus::prelude::*;

pub async fn public(method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return plain_not_found();
    }

    if uri.path().starts_with("/static/") {
        return plain_not_found();
    }

    if method == Method::HEAD {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            Body::empty(),
        )
            .into_response();
    }

    let html = render(|| {
        rsx! {
            crate::views::not_found::PublicNotFoundPage {
                signed_in_login: None::<String>,
            }
        }
    });
    (StatusCode::NOT_FOUND, Html(html)).into_response()
}

fn plain_not_found() -> Response {
    WebError::NotFound.into_response()
}
