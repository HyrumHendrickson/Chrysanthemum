//! End-to-end tests of the HTTP API and the collaboration socket.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use chrysanthemum::{
    api::{AppState, ShareInfo, router},
    collab::Collab,
    compile::CompileManager,
    guard::Guard,
    store::Store,
    toolchain::ToolchainManager,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yrs::{
    Doc, GetString, ReadTxn, Text, Transact, Update,
    updates::{decoder::Decode, encoder::Encode},
};

struct Harness {
    app: Router,
    state: AppState,
    _tmp: tempfile::TempDir,
}

fn harness(token: Option<&str>) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().to_path_buf()).unwrap();
    let state = AppState {
        store: store.clone(),
        collab: Collab::new(store.clone()),
        compiler: Arc::new(CompileManager::new(
            store,
            ToolchainManager::new(tmp.path()),
            Duration::from_secs(60),
        )),
        guard: Arc::new(Guard {
            token: token.map(str::to_string),
            loopback_only: true,
        }),
        share: Arc::new(ShareInfo::default()),
    };
    Harness {
        app: router(state.clone()),
        state,
        _tmp: tmp,
    }
}

fn req(method: Method, uri: &str, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "localhost:3000");
    let body = match body {
        Some(v) => {
            b = b.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    b.body(body).unwrap()
}

async fn send(app: &Router, r: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = app.clone().oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

#[tokio::test]
async fn project_and_file_lifecycle() {
    let h = harness(None);
    let (s, _) = send(
        &h.app,
        req(Method::POST, "/api/projects", Some(json!({"name": "demo"}))),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = send(
        &h.app,
        req(Method::POST, "/api/projects", Some(json!({"name": "demo"}))),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = send(
        &h.app,
        req(
            Method::POST,
            "/api/projects",
            Some(json!({"name": "../evil"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (_, body) = send(&h.app, req(Method::GET, "/api/projects", None)).await;
    assert_eq!(json_of(&body)[0]["id"], "demo");

    let (s, _) = send(
        &h.app,
        req(
            Method::POST,
            "/api/projects/demo/files",
            Some(json!({"path": "chapters/one.tex", "kind": "file"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = send(
        &h.app,
        req(
            Method::PUT,
            "/api/projects/demo/upload?path=img/a.bin",
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);

    let (_, body) = send(&h.app, req(Method::GET, "/api/projects/demo/files", None)).await;
    let files: Vec<String> = json_of(&body)
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_string())
        .collect();
    assert!(files.contains(&"main.tex".to_string()));
    assert!(files.contains(&"chapters/one.tex".to_string()));
    assert!(files.contains(&"img/a.bin".to_string()));

    let (s, _) = send(
        &h.app,
        req(
            Method::POST,
            "/api/projects/demo/files/rename",
            Some(json!({"from": "chapters/one.tex", "to": "chapters/two.tex"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = send(
        &h.app,
        req(
            Method::GET,
            "/api/projects/demo/raw?path=chapters/two.tex",
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // The main document is protected from deletion.
    let (s, _) = send(
        &h.app,
        req(
            Method::DELETE,
            "/api/projects/demo/files?path=main.tex",
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = send(
        &h.app,
        req(
            Method::DELETE,
            "/api/projects/demo/files?path=chapters",
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s, body) = send(
        &h.app,
        req(Method::GET, "/api/projects/demo/download.zip", None),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.starts_with(b"PK"));

    let (s, _) = send(&h.app, req(Method::DELETE, "/api/projects/demo", None)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = send(&h.app, req(Method::GET, "/api/projects/demo/files", None)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn path_traversal_is_rejected() {
    let h = harness(None);
    send(
        &h.app,
        req(Method::POST, "/api/projects", Some(json!({"name": "p"}))),
    )
    .await;
    for bad in [
        "../x",
        "/etc/passwd",
        "a/../../b",
        ".chrysanthemum/project.json",
    ] {
        let uri = format!("/api/projects/p/raw?path={}", bad.replace('/', "%2F"));
        let (s, _) = send(&h.app, req(Method::GET, &uri, None)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (s, _) = send(
        &h.app,
        req(Method::GET, "/api/projects/..%2Fetc/files", None),
    )
    .await;
    assert!(s == StatusCode::BAD_REQUEST || s == StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_foreign_host_and_origin() {
    let h = harness(None);
    let r = Request::builder()
        .uri("/api/health")
        .header(header::HOST, "evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&h.app, r).await.0, StatusCode::FORBIDDEN);

    let r = Request::builder()
        .method(Method::POST)
        .uri("/api/projects")
        .header(header::HOST, "localhost:3000")
        .header(header::ORIGIN, "http://evil.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"name": "x"}).to_string()))
        .unwrap();
    assert_eq!(send(&h.app, r).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn token_authentication() {
    let h = harness(Some("s3cret"));
    assert_eq!(
        send(&h.app, req(Method::GET, "/api/projects", None))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&h.app, req(Method::GET, "/", None)).await.0,
        StatusCode::UNAUTHORIZED
    );

    let mut r = req(Method::GET, "/api/projects", None);
    r.headers_mut()
        .insert(header::AUTHORIZATION, "Bearer s3cret".parse().unwrap());
    assert_eq!(send(&h.app, r).await.0, StatusCode::OK);

    let mut r = req(Method::GET, "/api/projects", None);
    r.headers_mut().insert(
        header::COOKIE,
        "chrysanthemum_token=s3cret".parse().unwrap(),
    );
    assert_eq!(send(&h.app, r).await.0, StatusCode::OK);

    let mut r = req(Method::GET, "/api/projects", None);
    r.headers_mut()
        .insert(header::COOKIE, "chrysanthemum_token=wrong".parse().unwrap());
    assert_eq!(send(&h.app, r).await.0, StatusCode::UNAUTHORIZED);

    // A browser visit with ?token= sets the cookie and redirects to a clean URL.
    let resp = h
        .app
        .clone()
        .oneshot(req(Method::GET, "/?token=s3cret", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(resp.headers()[header::LOCATION], "/");
    assert!(
        resp.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("chrysanthemum_token=s3cret")
    );
}

fn tex_available() -> bool {
    let tmp = tempfile::tempdir().unwrap();
    ToolchainManager::new(tmp.path())
        .current()
        .toolchain
        .pdflatex
}

#[tokio::test]
async fn compiles_valid_and_broken_documents() {
    if !tex_available() {
        eprintln!("skipping: no TeX installation");
        return;
    }
    let h = harness(None);
    send(
        &h.app,
        req(Method::POST, "/api/projects", Some(json!({"name": "doc"}))),
    )
    .await;

    let (s, body) = send(&h.app, req(Method::POST, "/api/projects/doc/compile", None)).await;
    assert_eq!(s, StatusCode::OK);
    let result = json_of(&body);
    assert_eq!(result["ok"], true, "log: {}", result["log"]);
    assert_eq!(result["pdf"], true);
    let (s, pdf) = send(&h.app, req(Method::GET, "/api/projects/doc/pdf", None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(pdf.starts_with(b"%PDF"));

    h.state
        .store
        .write_file(
            "doc",
            "main.tex",
            b"\\documentclass{article}\\begin{document}\\oops\\end{document}\n",
        )
        .unwrap();
    let (_, body) = send(&h.app, req(Method::POST, "/api/projects/doc/compile", None)).await;
    let result = json_of(&body);
    assert_eq!(result["ok"], false);
    let diags = result["diagnostics"].as_array().unwrap();
    assert!(
        diags
            .iter()
            .any(|d| d["severity"] == "error" && d["line"] == 1),
        "diagnostics: {diags:?}"
    );

    // Last result is retrievable and cleaning removes the PDF.
    let (_, body) = send(&h.app, req(Method::GET, "/api/projects/doc/compile", None)).await;
    assert_eq!(json_of(&body)["id"], 2);
    assert_eq!(
        send(&h.app, req(Method::POST, "/api/projects/doc/clean", None))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&h.app, req(Method::GET, "/api/projects/doc/pdf", None))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn compile_requires_an_existing_main_file() {
    let h = harness(None);
    send(
        &h.app,
        req(Method::POST, "/api/projects", Some(json!({"name": "m"}))),
    )
    .await;
    std::fs::remove_file(h.state.store.root().join("m/main.tex")).unwrap();
    let (s, _) = send(&h.app, req(Method::POST, "/api/projects/m/compile", None)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Real-time collaboration over an actual socket
// ---------------------------------------------------------------------------

fn var_u64(out: &mut Vec<u8>, mut v: u64) {
    while v > 0x7f {
        out.push(0x80 | (v & 0x7f) as u8);
        v >>= 7;
    }
    out.push(v as u8);
}

fn read_var(buf: &[u8], pos: &mut usize) -> u64 {
    let (mut r, mut shift) = (0u64, 0);
    loop {
        let b = buf[*pos];
        *pos += 1;
        r |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return r;
        }
        shift += 7;
    }
}

fn sync_msg(kind: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0];
    var_u64(&mut out, kind);
    var_u64(&mut out, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[allow(clippy::single_match)]
/// Applies server messages to `doc`; returns once `done` holds or time runs out.
async fn pump(ws: &mut Socket, doc: &Doc, done: impl Fn(&Doc) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if done(doc) {
            return;
        }
        let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_millis(200), ws.next()).await
        else {
            continue;
        };
        let tokio_tungstenite::tungstenite::Message::Binary(data) = msg else {
            continue;
        };
        let mut pos = 0;
        match read_var(&data, &mut pos) {
            0 => {
                let sub = read_var(&data, &mut pos);
                let len = read_var(&data, &mut pos) as usize;
                let payload = &data[pos..pos + len];
                match sub {
                    0 => {
                        let sv = yrs::StateVector::decode_v1(payload).unwrap();
                        let update = doc.transact().encode_state_as_update_v1(&sv);
                        ws.send(sync_msg(1, &update).into()).await.unwrap();
                    }
                    1 | 2 => {
                        doc.transact_mut()
                            .apply_update(Update::decode_v1(payload).unwrap())
                            .unwrap();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    panic!("timed out waiting for synchronisation");
}

#[tokio::test]
async fn two_clients_edit_the_same_document() {
    let h = harness(None);
    send(
        &h.app,
        req(Method::POST, "/api/projects", Some(json!({"name": "pair"}))),
    )
    .await;
    h.state
        .store
        .write_file("pair", "main.tex", b"hello")
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = h.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let url = format!("ws://{addr}/ws/doc/pair?path=main.tex");
    let (mut ws_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut ws_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (doc_a, doc_b) = (Doc::new(), Doc::new());
    let text_a = doc_a.get_or_insert_text("content");
    let text_b = doc_b.get_or_insert_text("content");

    // Like y-websocket, each client opens by sending its own sync request.
    for (ws, doc) in [(&mut ws_a, &doc_a), (&mut ws_b, &doc_b)] {
        let sv = doc.transact().state_vector().encode_v1();
        ws.send(sync_msg(0, &sv).into()).await.unwrap();
    }
    pump(&mut ws_a, &doc_a, |d| {
        d.get_or_insert_text("content").get_string(&d.transact()) == "hello"
    })
    .await;
    pump(&mut ws_b, &doc_b, |d| {
        d.get_or_insert_text("content").get_string(&d.transact()) == "hello"
    })
    .await;

    // A types; the edit must reach B.
    let before = doc_a.transact().state_vector();
    text_a.insert(&mut doc_a.transact_mut(), 5, ", world");
    let diff = doc_a.transact().encode_state_as_update_v1(&before);
    ws_a.send(sync_msg(2, &diff).into()).await.unwrap();
    pump(&mut ws_b, &doc_b, |d| {
        d.get_or_insert_text("content").get_string(&d.transact()) == "hello, world"
    })
    .await;
    assert_eq!(text_b.get_string(&doc_b.transact()), "hello, world");

    // The server writes the merged text back to the project file.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        h.state.store.read_file("pair", "main.tex").unwrap(),
        b"hello, world"
    );
}

// ---------------------------------------------------------------------------
// Compilers
// ---------------------------------------------------------------------------

fn from_peer(mut r: Request<Body>, addr: &str) -> Request<Body> {
    r.extensions_mut().insert(axum::extract::ConnectInfo(
        addr.parse::<std::net::SocketAddr>().unwrap(),
    ));
    r
}

#[tokio::test]
async fn lists_installable_compilers() {
    let h = harness(None);
    let (s, body) = send(&h.app, req(Method::GET, "/api/toolchain", None)).await;
    assert_eq!(s, StatusCode::OK);
    let v = json_of(&body);
    assert_eq!(v["can_manage"], true);
    let ids: Vec<&str> = v["compilers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["texlive", "tectonic"]);
    assert!(v["active"].get("pdflatex").is_some());

    let (_, health) = send(&h.app, req(Method::GET, "/api/health", None)).await;
    assert_eq!(json_of(&health)["toolchain"], v["active"]);
}

#[tokio::test]
async fn only_the_local_computer_may_change_compilers() {
    let h = harness(None);
    let remote = "192.168.1.50:5000";
    let (s, body) = send(
        &h.app,
        from_peer(req(Method::GET, "/api/toolchain", None), remote),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(json_of(&body)["can_manage"], false);

    for (method, uri) in [
        (Method::POST, "/api/toolchain/tectonic"),
        (Method::DELETE, "/api/toolchain/tectonic"),
        (Method::POST, "/api/toolchain/cancel"),
    ] {
        let (s, _) = send(&h.app, from_peer(req(method, uri, None), remote)).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
    }

    // The same requests from the loopback address are not refused for that reason.
    let local = "127.0.0.1:5000";
    let (s, _) = send(
        &h.app,
        from_peer(req(Method::DELETE, "/api/toolchain/tectonic", None), local),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "nothing is installed to remove");
    let (s, _) = send(
        &h.app,
        from_peer(req(Method::POST, "/api/toolchain/cancel", None), local),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "nothing is running to cancel");
    let (s, _) = send(
        &h.app,
        from_peer(req(Method::POST, "/api/toolchain/nonsense", None), local),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
