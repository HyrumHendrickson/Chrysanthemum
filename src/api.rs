//! HTTP and WebSocket routes.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State, WebSocketUpgrade, ws::Message},
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::Response,
    routing::{delete, get, post, put},
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::{
    compression::CompressionLayer, set_header::SetResponseHeaderLayer, trace::TraceLayer,
};

use crate::{
    assets,
    collab::Collab,
    compile::CompileManager,
    error::{AppError, AppResult},
    guard::{Guard, guard},
    store::{Entry, EntryKind, ProjectInfo, ProjectSettings, Store, clean_rel_path, is_text},
    toolchain::{CompilerId, JobStatus, ToolchainStatus},
};

const MAX_UPLOAD: usize = 100 * 1024 * 1024;

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self' ws: wss:; \
worker-src 'self' blob:; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// Information shown in the "Share" dialog.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ShareInfo {
    pub lan: bool,
    pub urls: Vec<String>,
    pub token: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub collab: Arc<Collab>,
    pub compiler: Arc<CompileManager>,
    pub guard: Arc<Guard>,
    pub share: Arc<ShareInfo>,
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/share", get(share))
        .route("/toolchain", get(toolchain_status))
        .route(
            "/toolchain/{id}",
            post(toolchain_install).delete(toolchain_remove),
        )
        .route("/toolchain/cancel", post(toolchain_cancel))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{id}", delete(delete_project))
        .route(
            "/projects/{id}/settings",
            get(get_settings).put(put_settings),
        )
        .route(
            "/projects/{id}/files",
            get(list_files).post(create_entry).delete(delete_entry),
        )
        .route("/projects/{id}/files/rename", post(rename_entry))
        .route("/projects/{id}/raw", get(raw_file))
        .route(
            "/projects/{id}/upload",
            put(upload_file).layer(DefaultBodyLimit::max(MAX_UPLOAD)),
        )
        .route("/projects/{id}/compile", get(last_compile).post(compile))
        .route("/projects/{id}/clean", post(clean))
        .route("/projects/{id}/pdf", get(pdf))
        .route("/projects/{id}/download.zip", get(download_zip));

    Router::new()
        .nest("/api", api)
        .route("/ws/doc/{id}", get(ws_doc))
        .route("/ws/events/{id}", get(ws_events))
        .fallback(assets::serve)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::Internal(e.into()))?
}

// ---------------------------------------------------------------------------
// Meta
// ---------------------------------------------------------------------------

async fn health(State(st): State<AppState>) -> Json<serde_json::Value> {
    let resolved = st.compiler.toolchains().current();
    Json(json!({
        "name": "chrysanthemum",
        "version": env!("CARGO_PKG_VERSION"),
        "toolchain": resolved.toolchain,
        "toolchain_ok": resolved.toolchain.any(),
    }))
}

// ---------------------------------------------------------------------------
// Compilers
// ---------------------------------------------------------------------------

/// The address of the connecting client, when the server knows it.
type Peer = Option<Extension<ConnectInfo<SocketAddr>>>;

/// Only the computer that runs the server may change which compilers are
/// installed; collaborators on the network may look but not touch.
fn is_local(peer: &Peer) -> bool {
    peer.as_ref()
        .is_none_or(|Extension(ConnectInfo(addr))| addr.ip().to_canonical().is_loopback())
}

fn require_local(peer: &Peer) -> AppResult<()> {
    if is_local(peer) {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "compilers can only be installed or removed from the computer running Chrysanthemum",
        ))
    }
}

async fn toolchain_status(State(st): State<AppState>, peer: Peer) -> Json<ToolchainStatus> {
    Json(st.compiler.toolchains().status(is_local(&peer)))
}

async fn toolchain_install(
    State(st): State<AppState>,
    peer: Peer,
    Path(id): Path<CompilerId>,
) -> AppResult<(StatusCode, Json<JobStatus>)> {
    require_local(&peer)?;
    let status = st.compiler.toolchains().install(id)?;
    Ok((StatusCode::ACCEPTED, Json(status)))
}

async fn toolchain_cancel(State(st): State<AppState>, peer: Peer) -> AppResult<StatusCode> {
    require_local(&peer)?;
    if st.compiler.toolchains().cancel() {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(AppError::not_found("no installation is running"))
    }
}

async fn toolchain_remove(
    State(st): State<AppState>,
    peer: Peer,
    Path(id): Path<CompilerId>,
) -> AppResult<StatusCode> {
    require_local(&peer)?;
    let toolchains = st.compiler.toolchains().clone();
    blocking(move || toolchains.remove(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn share(State(st): State<AppState>) -> Json<ShareInfo> {
    Json((*st.share).clone())
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

async fn list_projects(State(st): State<AppState>) -> AppResult<Json<Vec<ProjectInfo>>> {
    Ok(Json(blocking(move || st.store.list_projects()).await?))
}

#[derive(Deserialize)]
struct CreateProject {
    name: String,
}

async fn create_project(
    State(st): State<AppState>,
    Json(body): Json<CreateProject>,
) -> AppResult<(StatusCode, Json<ProjectInfo>)> {
    let name = body.name.trim().to_string();
    let info = blocking(move || st.store.create_project(&name)).await?;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn delete_project(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    st.collab.evict_project(&id).await;
    st.compiler.forget(&id);
    let store = st.store.clone();
    blocking(move || store.delete_project(&id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_settings(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<ProjectSettings>> {
    Ok(Json(blocking(move || st.store.load_settings(&id)).await?))
}

async fn put_settings(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(settings): Json<ProjectSettings>,
) -> AppResult<Json<ProjectSettings>> {
    let store = st.store.clone();
    let pid = id.clone();
    let saved = blocking(move || {
        store.save_settings(&pid, &settings)?;
        store.load_settings(&pid)
    })
    .await?;
    st.collab.publish_event(&id, &json!({ "type": "settings" }));
    Ok(Json(saved))
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

async fn list_files(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Vec<Entry>>> {
    Ok(Json(blocking(move || st.store.list_files(&id)).await?))
}

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

#[derive(Deserialize)]
struct CreateEntry {
    path: String,
    kind: EntryKind,
}

async fn create_entry(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CreateEntry>,
) -> AppResult<StatusCode> {
    let store = st.store.clone();
    let pid = id.clone();
    blocking(move || store.create_entry(&pid, &body.path, body.kind)).await?;
    st.collab.publish_event(&id, &json!({ "type": "files" }));
    Ok(StatusCode::CREATED)
}

async fn delete_entry(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> AppResult<StatusCode> {
    let path = clean_rel_path(&q.path)?;
    let settings = st.store.load_settings(&id)?;
    if settings.main == path {
        return Err(AppError::conflict(
            "the main document cannot be deleted; choose another main file first",
        ));
    }
    st.collab.evict(&id, &path, false).await;
    let store = st.store.clone();
    let pid = id.clone();
    blocking(move || store.delete_entry(&pid, &path)).await?;
    st.collab.publish_event(&id, &json!({ "type": "files" }));
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Rename {
    from: String,
    to: String,
}

async fn rename_entry(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Rename>,
) -> AppResult<StatusCode> {
    let from = clean_rel_path(&body.from)?;
    let to = clean_rel_path(&body.to)?;
    st.collab.evict(&id, &from, true).await;
    let store = st.store.clone();
    let pid = id.clone();
    blocking(move || {
        store.rename_entry(&pid, &from, &to)?;
        let mut settings = store.load_settings(&pid)?;
        if settings.main == from {
            settings.main = to;
            store.save_settings(&pid, &settings)?;
        }
        Ok(())
    })
    .await?;
    st.collab.publish_event(&id, &json!({ "type": "files" }));
    st.collab.publish_event(&id, &json!({ "type": "settings" }));
    Ok(StatusCode::NO_CONTENT)
}

async fn raw_file(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> AppResult<Response> {
    let path = q.path.clone();
    let bytes = blocking(move || st.store.read_file(&id, &path)).await?;
    let mime = mime_guess::from_path(&q.path).first_or_octet_stream();
    let inline = mime.type_() == mime_guess::mime::IMAGE && mime.subtype() != "svg+xml";
    let disposition = if inline { "inline" } else { "attachment" };
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            if is_text(&bytes) && !inline {
                "text/plain; charset=utf-8".to_string()
            } else {
                mime.to_string()
            },
        )
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "no-store")
        .header(
            header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'",
        )
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(e.into()))
}

async fn upload_file(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
    body: Bytes,
) -> AppResult<StatusCode> {
    let path = clean_rel_path(&q.path)?;
    if st.collab.is_open(&id, &path).await {
        return Err(AppError::conflict(
            "that file is being edited; close it first",
        ));
    }
    let store = st.store.clone();
    let pid = id.clone();
    blocking(move || store.write_file(&pid, &path, &body)).await?;
    st.collab.publish_event(&id, &json!({ "type": "files" }));
    Ok(StatusCode::CREATED)
}

async fn download_zip(State(st): State<AppState>, Path(id): Path<String>) -> AppResult<Response> {
    st.collab.flush_project(&id).await;
    let pid = id.clone();
    let data = blocking(move || st.store.zip_project(&pid)).await?;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{id}.zip\""),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(data))
        .map_err(|e| AppError::Internal(e.into()))
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

fn summary(result: &crate::compile::CompileResult) -> serde_json::Value {
    let mut v = serde_json::to_value(result).unwrap_or_default();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("log");
    }
    v
}

async fn compile(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<crate::compile::CompileResult>> {
    st.store.project_dir(&id)?;
    st.collab.flush_project(&id).await;
    let result = st.compiler.compile(&id).await?;
    st.collab.publish_event(
        &id,
        &json!({ "type": "compiled", "result": summary(&result) }),
    );
    Ok(Json((*result).clone()))
}

async fn last_compile(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Option<crate::compile::CompileResult>>> {
    st.store.project_dir(&id)?;
    Ok(Json(st.compiler.last_result(&id).map(|r| (*r).clone())))
}

async fn clean(State(st): State<AppState>, Path(id): Path<String>) -> AppResult<StatusCode> {
    st.store.project_dir(&id)?;
    st.compiler.clean(&id).await?;
    st.collab.publish_event(&id, &json!({ "type": "cleaned" }));
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PdfQuery {
    #[serde(default)]
    download: bool,
}

async fn pdf(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PdfQuery>,
) -> AppResult<Response> {
    let path = st.compiler.pdf_path(&id)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| AppError::not_found("no PDF yet; compile the project first"))?;
    let mut resp = Response::builder()
        .header(header::CONTENT_TYPE, "application/pdf")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'");
    if q.download {
        resp = resp.header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{id}.pdf\""),
        );
    }
    resp.body(Body::from(bytes))
        .map_err(|e| AppError::Internal(e.into()))
}

// ---------------------------------------------------------------------------
// WebSockets
// ---------------------------------------------------------------------------

async fn ws_doc(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    // Fail with a proper HTTP status before upgrading.
    let path = clean_rel_path(&q.path)?;
    let probe = st.store.resolve(&id, &path)?;
    if !probe.is_file() {
        return Err(AppError::not_found("file not found"));
    }
    let collab = st.collab.clone();
    Ok(ws
        .max_message_size(32 * 1024 * 1024)
        .on_upgrade(move |socket| async move {
            match collab.join(&id, &path).await {
                Ok(room) => collab.serve_socket(socket, room).await,
                Err(e) => {
                    tracing::debug!("refusing document socket: {e}");
                    drop(socket);
                }
            }
        }))
}

async fn ws_events(
    State(st): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    st.store.project_dir(&id)?;
    let mut rx = st.collab.subscribe_events(&id);
    Ok(ws.on_upgrade(move |mut socket| async move {
        loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Ok(text) => {
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Tell the client to re-fetch everything.
                        let _ = socket.send(Message::Text(json!({"type":"resync"}).to_string().into())).await;
                    }
                    Err(_) => break,
                },
                incoming = socket.next() => match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
            }
        }
    }))
}
