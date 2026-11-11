//! Static web UI, embedded into the binary at build time.

use axum::{
    body::Body,
    http::{HeaderMap, StatusCode, Uri, header},
    response::Response,
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Assets;

/// Serves embedded files, falling back to `index.html` for client-side routes.
pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let candidate = if path.is_empty() { "index.html" } else { path };
    if let Some(resp) = respond(candidate, &headers) {
        return resp;
    }
    // Unknown paths with a file extension are genuine misses.
    if candidate
        .rsplit('/')
        .next()
        .is_some_and(|n| n.contains('.'))
    {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("not found"))
            .unwrap();
    }
    respond("index.html", &headers).unwrap_or_else(|| {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("web interface not built"))
            .unwrap()
    })
}

fn respond(path: &str, headers: &HeaderMap) -> Option<Response> {
    let file = Assets::get(path)?;
    let etag = format!(
        "\"{}\"",
        file.metadata
            .sha256_hash()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == etag)
    {
        return Some(
            Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(header::ETAG, etag)
                .body(Body::empty())
                .unwrap(),
        );
    }
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Some(
        Response::builder()
            .header(header::CONTENT_TYPE, mime.as_ref())
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(file.data.into_owned()))
            .unwrap(),
    )
}
