//! Request guard: host/origin validation and optional access-token authentication.

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::api::AppState;

pub const COOKIE_NAME: &str = "scriptorium_token";

/// Security policy derived from the server configuration.
#[derive(Debug, Clone)]
pub struct Guard {
    /// Required access token, if any.
    pub token: Option<String>,
    /// When true the server is bound to loopback only and requests must carry a
    /// loopback `Host` header (defends against DNS rebinding).
    pub loopback_only: bool,
}

pub async fn guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let g = &state.guard;
    let headers = req.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if g.loopback_only && !host_is_loopback(&host) {
        return (StatusCode::FORBIDDEN, "forbidden: unexpected Host header").into_response();
    }

    let is_ws = headers.contains_key(header::UPGRADE);
    let changes_state = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if (is_ws || changes_state)
        && let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && !origin_matches_host(origin, &host)
    {
        return (StatusCode::FORBIDDEN, "forbidden: cross-origin request").into_response();
    }

    let Some(token) = g.token.as_deref() else {
        return next.run(req).await;
    };

    if token_from_headers(headers).is_some_and(|t| ct_eq(&t, token)) {
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();
    let programmatic = path.starts_with("/api/") || path.starts_with("/ws/");
    if let Some(q) = token_from_query(req.uri().query())
        && ct_eq(&q, token)
    {
        if programmatic {
            return next.run(req).await;
        }
        // Browser navigation with ?token=…: store it in a cookie and clean the URL.
        let location = strip_token_param(&path, req.uri().query());
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(header::LOCATION, location)
            .header(
                header::SET_COOKIE,
                format!(
                    "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000"
                ),
            )
            .body(Body::empty())
            .unwrap();
    }

    if programmatic {
        return (StatusCode::UNAUTHORIZED, "{\"error\":\"unauthorized\"}").into_response();
    }
    login_page()
}

fn login_page() -> Response {
    const HTML: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>Scriptorium</title>
<style>body{font:16px system-ui,sans-serif;display:grid;place-items:center;min-height:100vh;margin:0;background:#f3f5f8;color:#1b2a41}
form{background:#fff;padding:2rem;border-radius:8px;box-shadow:0 1px 3px #0002;max-width:22rem;width:calc(100% - 2rem)}
h1{font-size:1.25rem;margin:0 0 .5rem}p{margin:0 0 1rem;color:#4a5a73}
input{width:100%;box-sizing:border-box;padding:.6rem;border:1px solid #b8c2d1;border-radius:6px;font:inherit}
button{margin-top:.75rem;width:100%;padding:.6rem;border:0;border-radius:6px;background:#2f5da8;color:#fff;font:inherit;cursor:pointer}
button:focus-visible,input:focus-visible{outline:2px solid #2f5da8;outline-offset:2px}</style></head>
<body><form method="get" action="/"><h1>Access token required</h1>
<p>Enter the token shown in the terminal where Scriptorium is running, or open the join link it printed.</p>
<input name="token" type="password" autocomplete="off" autofocus required aria-label="Access token">
<button type="submit">Open Scriptorium</button></form></body></html>"#;
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(HTML))
        .unwrap()
}

fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(v.trim().to_string());
    }
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == COOKIE_NAME).then(|| v.to_string())
    })
}

fn token_from_query(query: Option<&str>) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == "token").then(|| v.to_string())
    })
}

fn strip_token_param(path: &str, query: Option<&str>) -> String {
    let rest: Vec<&str> = query
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty() && !p.starts_with("token="))
        .collect();
    if rest.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", rest.join("&"))
    }
}

/// Constant-time string comparison.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

pub fn host_is_loopback(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else if host.matches(':').count() == 1 {
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    } else {
        host
    };
    if name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    name.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

fn origin_matches_host(origin: &str, host: &str) -> bool {
    let without_scheme = origin.split_once("://").map(|(_, r)| r).unwrap_or(origin);
    without_scheme.eq_ignore_ascii_case(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts() {
        assert!(host_is_loopback("localhost:3000"));
        assert!(host_is_loopback("127.0.0.1:3000"));
        assert!(host_is_loopback("[::1]:3000"));
        assert!(host_is_loopback("localhost"));
        assert!(!host_is_loopback("evil.example:3000"));
        assert!(!host_is_loopback("192.168.1.5:3000"));
    }

    #[test]
    fn origin_check() {
        assert!(origin_matches_host(
            "http://localhost:3000",
            "localhost:3000"
        ));
        assert!(!origin_matches_host(
            "http://evil.example",
            "localhost:3000"
        ));
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }

    #[test]
    fn token_param_stripping() {
        assert_eq!(strip_token_param("/", Some("token=x")), "/");
        assert_eq!(
            strip_token_param("/p", Some("a=1&token=x&b=2")),
            "/p?a=1&b=2"
        );
    }
}
