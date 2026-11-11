//! Real-time collaboration.
//!
//! Every open text file is backed by a [Yjs](https://yjs.dev) document hosted
//! by the server (via the `yrs` crate). Browsers speak the standard
//! `y-websocket` protocol, so edits merge conflict-free (CRDT) and presence
//! information (cursors, names) is relayed between peers. The server persists
//! the document text back to the project file shortly after each change and
//! flushes it before every compilation.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, Notify, broadcast};
use yrs::{
    Doc, GetString, ReadTxn, StateVector, Text, Transact, Update,
    updates::{decoder::Decode, encoder::Encode},
};

use crate::{
    error::{AppError, AppResult},
    store::{Store, is_text},
};

const TEXT_KEY: &str = "content";
const SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

const MSG_SYNC: u64 = 0;
/// Custom message announcing the identity ("epoch") of the server document.
/// Clients that see the epoch change must discard their local copy.
const MSG_EPOCH: u64 = 100;
const YDOC_MAGIC: &[u8; 4] = b"SCY1";
const MSG_AWARENESS: u64 = 1;
const SYNC_STEP1: u64 = 0;
const SYNC_STEP2: u64 = 1;
const SYNC_UPDATE: u64 = 2;

// ---------------------------------------------------------------------------
// lib0 variable-length integer helpers
// ---------------------------------------------------------------------------

fn write_var_u64(out: &mut Vec<u8>, mut v: u64) {
    while v > 0x7f {
        out.push(0x80 | (v & 0x7f) as u8);
        v >>= 7;
    }
    out.push(v as u8);
}

fn read_var_u64(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        result |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

fn write_var_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_var_u64(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn read_var_bytes<'a>(buf: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let len = usize::try_from(read_var_u64(buf, pos)?).ok()?;
    let end = pos.checked_add(len)?;
    let slice = buf.get(*pos..end)?;
    *pos = end;
    Some(slice)
}

fn sync_message(kind: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    write_var_u64(&mut out, MSG_SYNC);
    write_var_u64(&mut out, kind);
    write_var_bytes(&mut out, payload);
    out
}

/// One awareness entry: `(client id, clock, JSON state)`.
type AwarenessEntry = (u64, u64, Vec<u8>);

fn awareness_message(entries: &[AwarenessEntry]) -> Vec<u8> {
    let mut payload = Vec::new();
    write_var_u64(&mut payload, entries.len() as u64);
    for (id, clock, state) in entries {
        write_var_u64(&mut payload, *id);
        write_var_u64(&mut payload, *clock);
        write_var_bytes(&mut payload, state);
    }
    let mut out = Vec::new();
    write_var_u64(&mut out, MSG_AWARENESS);
    write_var_bytes(&mut out, &payload);
    out
}

fn parse_awareness(payload: &[u8]) -> Option<Vec<AwarenessEntry>> {
    let mut pos = 0;
    let count = read_var_u64(payload, &mut pos)?;
    let mut out = Vec::new();
    for _ in 0..count.min(1024) {
        let id = read_var_u64(payload, &mut pos)?;
        let clock = read_var_u64(payload, &mut pos)?;
        let state = read_var_bytes(payload, &mut pos)?.to_vec();
        out.push((id, clock, state));
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Rooms
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Fanout {
    Data { from: u64, bytes: Vec<u8> },
    Evict,
}

/// Per-connection state used to clean up presence when a peer leaves.
#[derive(Default)]
struct Conn {
    id: u64,
    awareness_ids: HashMap<u64, u64>,
}

pub struct Room {
    store: Store,
    project: String,
    path: String,
    epoch: u64,
    doc: StdMutex<Doc>,
    tx: broadcast::Sender<Fanout>,
    awareness: StdMutex<HashMap<u64, (u64, Vec<u8>)>>,
    dirty: AtomicBool,
    closed: AtomicBool,
    clients: AtomicUsize,
    notify: Notify,
}

impl Room {
    fn new(
        store: Store,
        project: &str,
        path: &str,
        initial: &str,
        persisted: Option<Vec<u8>>,
    ) -> Arc<Self> {
        // Prefer the persisted CRDT state when it still matches the file on
        // disk: that keeps the document's history (and its epoch) stable across
        // server restarts, so reconnecting clients merge instead of duplicating.
        let restored = persisted.and_then(|bytes| restore_doc(&bytes, initial));
        let (doc, epoch) = restored.unwrap_or_else(|| {
            let doc = Doc::new();
            {
                let text = doc.get_or_insert_text(TEXT_KEY);
                let mut txn = doc.transact_mut();
                text.insert(&mut txn, 0, initial);
            }
            (doc, rand::random::<u64>())
        });
        let (tx, _) = broadcast::channel(512);
        Arc::new(Self {
            store,
            project: project.to_string(),
            path: path.to_string(),
            epoch,
            doc: StdMutex::new(doc),
            tx,
            awareness: Default::default(),
            dirty: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
            notify: Notify::new(),
        })
    }

    fn epoch_message(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_var_u64(&mut out, MSG_EPOCH);
        write_var_bytes(&mut out, self.epoch.to_string().as_bytes());
        out
    }

    fn encode_persisted(&self) -> Vec<u8> {
        let update = {
            let doc = self.doc.lock().unwrap();
            doc.transact()
                .encode_state_as_update_v1(&StateVector::default())
        };
        let mut out = Vec::with_capacity(update.len() + 12);
        out.extend_from_slice(YDOC_MAGIC);
        out.extend_from_slice(&self.epoch.to_le_bytes());
        out.extend_from_slice(&update);
        out
    }

    fn state_vector(&self) -> Vec<u8> {
        let doc = self.doc.lock().unwrap();
        doc.transact().state_vector().encode_v1()
    }

    fn text(&self) -> String {
        let doc = self.doc.lock().unwrap();
        let text = doc.get_or_insert_text(TEXT_KEY);
        text.get_string(&doc.transact())
    }

    /// Writes the document to disk if it changed since the last save.
    pub fn save(&self) {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return;
        }
        let text = self.text();
        if let Err(e) = self
            .store
            .write_file(&self.project, &self.path, text.as_bytes())
        {
            tracing::error!(project = %self.project, path = %self.path, "failed to save: {e}");
            self.dirty.store(true, Ordering::SeqCst);
            return;
        }
        if let Err(e) = self
            .store
            .save_ydoc(&self.project, &self.path, &self.encode_persisted())
        {
            tracing::warn!(project = %self.project, path = %self.path, "failed to persist document state: {e}");
        }
    }

    /// Processes one protocol message from a client and returns direct replies.
    fn handle(&self, conn: &mut Conn, data: &[u8]) -> Vec<Vec<u8>> {
        let mut pos = 0;
        let Some(kind) = read_var_u64(data, &mut pos) else {
            return Vec::new();
        };
        match kind {
            MSG_SYNC => {
                let Some(sub) = read_var_u64(data, &mut pos) else {
                    return Vec::new();
                };
                let Some(payload) = read_var_bytes(data, &mut pos) else {
                    return Vec::new();
                };
                match sub {
                    SYNC_STEP1 => {
                        let Ok(sv) = StateVector::decode_v1(payload) else {
                            return Vec::new();
                        };
                        let update = {
                            let doc = self.doc.lock().unwrap();
                            doc.transact().encode_state_as_update_v1(&sv)
                        };
                        vec![sync_message(SYNC_STEP2, &update)]
                    }
                    SYNC_STEP2 | SYNC_UPDATE => {
                        let Ok(update) = Update::decode_v1(payload) else {
                            return Vec::new();
                        };
                        let applied = {
                            let doc = self.doc.lock().unwrap();
                            let mut txn = doc.transact_mut();
                            txn.apply_update(update).is_ok()
                        };
                        if applied {
                            self.dirty.store(true, Ordering::SeqCst);
                            self.notify.notify_one();
                            let _ = self.tx.send(Fanout::Data {
                                from: conn.id,
                                bytes: sync_message(SYNC_UPDATE, payload),
                            });
                        }
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            MSG_AWARENESS => {
                let Some(payload) = read_var_bytes(data, &mut pos) else {
                    return Vec::new();
                };
                let Some(entries) = parse_awareness(payload) else {
                    return Vec::new();
                };
                {
                    let mut cache = self.awareness.lock().unwrap();
                    for (id, clock, state) in &entries {
                        conn.awareness_ids.insert(*id, *clock);
                        if state.as_slice() == b"null" {
                            cache.remove(id);
                        } else {
                            cache.insert(*id, (*clock, state.clone()));
                        }
                    }
                }
                let _ = self.tx.send(Fanout::Data {
                    from: conn.id,
                    bytes: data.to_vec(),
                });
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn awareness_snapshot(&self) -> Option<Vec<u8>> {
        let cache = self.awareness.lock().unwrap();
        if cache.is_empty() {
            return None;
        }
        let entries: Vec<AwarenessEntry> = cache
            .iter()
            .map(|(id, (clock, state))| (*id, *clock, state.clone()))
            .collect();
        Some(awareness_message(&entries))
    }

    /// Announces that every presence entry owned by `conn` has gone away.
    fn drop_presence(&self, conn: &Conn) {
        let mut entries = Vec::new();
        {
            let mut cache = self.awareness.lock().unwrap();
            for (id, clock) in &conn.awareness_ids {
                if cache.remove(id).is_some() {
                    entries.push((*id, clock + 1, b"null".to_vec()));
                }
            }
        }
        if !entries.is_empty() {
            let _ = self.tx.send(Fanout::Data {
                from: conn.id,
                bytes: awareness_message(&entries),
            });
        }
    }
}

/// Rebuilds a document from persisted state if its text equals `disk_text`.
fn restore_doc(bytes: &[u8], disk_text: &str) -> Option<(Doc, u64)> {
    if bytes.len() < 12 || &bytes[..4] != YDOC_MAGIC {
        return None;
    }
    let epoch = u64::from_le_bytes(bytes[4..12].try_into().ok()?);
    let update = Update::decode_v1(&bytes[12..]).ok()?;
    let doc = Doc::new();
    let text = doc.get_or_insert_text(TEXT_KEY);
    {
        let mut txn = doc.transact_mut();
        txn.apply_update(update).ok()?;
    }
    let matches = text.get_string(&doc.transact()) == disk_text;
    matches.then_some((doc, epoch))
}

type RoomKey = (String, String);

/// Registry of live rooms plus per-project event channels.
pub struct Collab {
    store: Store,
    rooms: Mutex<HashMap<RoomKey, Arc<Room>>>,
    events: StdMutex<HashMap<String, broadcast::Sender<String>>>,
    next_conn: AtomicU64,
}

impl Collab {
    pub fn new(store: Store) -> Arc<Self> {
        Arc::new(Self {
            store,
            rooms: Default::default(),
            events: Default::default(),
            next_conn: AtomicU64::new(1),
        })
    }

    /// Joins (creating if needed) the room for a project file.
    pub async fn join(&self, project: &str, path: &str) -> AppResult<Arc<Room>> {
        let path = crate::store::clean_rel_path(path)?;
        let key = (project.to_string(), path.clone());
        let mut rooms = self.rooms.lock().await;
        if let Some(room) = rooms.get(&key) {
            room.clients.fetch_add(1, Ordering::SeqCst);
            return Ok(room.clone());
        }
        let bytes = self.store.read_file(project, &path)?;
        if !is_text(&bytes) {
            return Err(AppError::bad_request("file is not a text file"));
        }
        let initial =
            String::from_utf8(bytes).map_err(|_| AppError::bad_request("file is not UTF-8"))?;
        let persisted = self.store.load_ydoc(project, &path);
        let room = Room::new(self.store.clone(), project, &path, &initial, persisted);
        room.clients.store(1, Ordering::SeqCst);
        rooms.insert(key, room.clone());
        let saver = room.clone();
        tokio::spawn(async move {
            loop {
                saver.notify.notified().await;
                if saver.closed.load(Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(SAVE_DEBOUNCE).await;
                let r = saver.clone();
                let _ = tokio::task::spawn_blocking(move || r.save()).await;
            }
        });
        Ok(room)
    }

    async fn leave(&self, room: &Arc<Room>) {
        let key = (room.project.clone(), room.path.clone());
        let last = {
            let mut rooms = self.rooms.lock().await;
            let remaining = room.clients.fetch_sub(1, Ordering::SeqCst) - 1;
            if remaining == 0 {
                if rooms.get(&key).is_some_and(|r| Arc::ptr_eq(r, room)) {
                    rooms.remove(&key);
                }
                room.closed.store(true, Ordering::SeqCst);
                true
            } else {
                false
            }
        };
        if last {
            room.notify.notify_one();
            let r = room.clone();
            let _ = tokio::task::spawn_blocking(move || r.save()).await;
        }
    }

    /// Persists every open document (called on shutdown).
    pub async fn flush_all(&self) {
        let rooms: Vec<Arc<Room>> = self.rooms.lock().await.values().cloned().collect();
        for room in rooms {
            let _ = tokio::task::spawn_blocking(move || room.save()).await;
        }
    }

    /// True while somebody has the file open for editing.
    pub async fn is_open(&self, project: &str, path: &str) -> bool {
        let key = (project.to_string(), path.to_string());
        self.rooms.lock().await.contains_key(&key)
    }

    /// Persists every pending change of a project (called before compiling).
    pub async fn flush_project(&self, project: &str) {
        let rooms: Vec<Arc<Room>> = {
            let rooms = self.rooms.lock().await;
            rooms
                .iter()
                .filter(|((p, _), _)| p == project)
                .map(|(_, r)| r.clone())
                .collect()
        };
        for room in rooms {
            let _ = tokio::task::spawn_blocking(move || room.save()).await;
        }
    }

    /// Disconnects editors of `path` (and anything beneath it) after the
    /// file was renamed or deleted. Pending changes are flushed first when
    /// `save` is set.
    pub async fn evict(&self, project: &str, path: &str, save: bool) {
        let prefix = format!("{path}/");
        let victims: Vec<Arc<Room>> = {
            let mut rooms = self.rooms.lock().await;
            let keys: Vec<RoomKey> = rooms
                .keys()
                .filter(|(p, f)| p == project && (f == path || f.starts_with(&prefix)))
                .cloned()
                .collect();
            keys.into_iter().filter_map(|k| rooms.remove(&k)).collect()
        };
        self.store.remove_ydoc(project, path);
        for room in victims {
            if save {
                let r = room.clone();
                let _ = tokio::task::spawn_blocking(move || r.save()).await;
            }
            room.dirty.store(false, Ordering::SeqCst);
            room.closed.store(true, Ordering::SeqCst);
            room.notify.notify_one();
            let _ = room.tx.send(Fanout::Evict);
        }
    }

    /// Drops all rooms of a project (used when the project is deleted).
    pub async fn evict_project(&self, project: &str) {
        let victims: Vec<Arc<Room>> = {
            let mut rooms = self.rooms.lock().await;
            let keys: Vec<RoomKey> = rooms
                .keys()
                .filter(|(p, _)| p == project)
                .cloned()
                .collect();
            keys.into_iter().filter_map(|k| rooms.remove(&k)).collect()
        };
        for room in victims {
            room.dirty.store(false, Ordering::SeqCst);
            room.closed.store(true, Ordering::SeqCst);
            room.notify.notify_one();
            let _ = room.tx.send(Fanout::Evict);
        }
        self.events.lock().unwrap().remove(project);
    }

    // ----- project event channel (compile results, file tree changes) -----

    pub fn subscribe_events(&self, project: &str) -> broadcast::Receiver<String> {
        self.events
            .lock()
            .unwrap()
            .entry(project.to_string())
            .or_insert_with(|| broadcast::channel(64).0)
            .subscribe()
    }

    pub fn publish_event(&self, project: &str, event: &serde_json::Value) {
        if let Some(tx) = self.events.lock().unwrap().get(project) {
            let _ = tx.send(event.to_string());
        }
    }

    /// Serves one WebSocket connection attached to `room`.
    pub async fn serve_socket(self: Arc<Self>, socket: WebSocket, room: Arc<Room>) {
        let mut conn = Conn {
            id: self.next_conn.fetch_add(1, Ordering::SeqCst),
            ..Conn::default()
        };
        let mut rx = room.tx.subscribe();
        let (mut sink, mut stream) = socket.split();

        // Open with our state vector so the client can send what we lack, and
        // with a snapshot of who else is present.
        let mut hello = vec![
            room.epoch_message(),
            sync_message(SYNC_STEP1, &room.state_vector()),
        ];
        hello.extend(room.awareness_snapshot());
        let mut alive = true;
        for msg in hello {
            if sink.send(Message::Binary(msg.into())).await.is_err() {
                alive = false;
                break;
            }
        }

        while alive {
            tokio::select! {
                incoming = stream.next() => match incoming {
                    Some(Ok(Message::Binary(data))) => {
                        let replies = room.handle(&mut conn, &data);
                        for reply in replies {
                            if sink.send(Message::Binary(reply.into())).await.is_err() {
                                alive = false;
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
                outgoing = rx.recv() => match outgoing {
                    Ok(Fanout::Data { from, bytes }) => {
                        if from != conn.id && sink.send(Message::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(Fanout::Evict) => break,
                    // A slow client missed updates; reconnecting resynchronises it.
                    Err(broadcast::error::RecvError::Lagged(_)) => break,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
        let _ = sink.close().await;
        room.drop_presence(&conn);
        self.leave(&room).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [
            0u64,
            1,
            127,
            128,
            300,
            16_383,
            16_384,
            u32::MAX as u64,
            1 << 40,
        ] {
            let mut buf = Vec::new();
            write_var_u64(&mut buf, v);
            let mut pos = 0;
            assert_eq!(read_var_u64(&buf, &mut pos), Some(v));
            assert_eq!(pos, buf.len());
        }
        assert_eq!(read_var_u64(&[0x80], &mut 0), None);
    }

    #[test]
    fn bytes_roundtrip_and_truncation() {
        let mut buf = Vec::new();
        write_var_bytes(&mut buf, b"hello");
        let mut pos = 0;
        assert_eq!(read_var_bytes(&buf, &mut pos), Some(&b"hello"[..]));
        assert_eq!(read_var_bytes(&buf[..3], &mut 0), None);
    }

    #[test]
    fn awareness_roundtrip() {
        let entries = vec![(7u64, 3u64, br#"{"user":"a"}"#.to_vec())];
        let msg = awareness_message(&entries);
        let mut pos = 0;
        assert_eq!(read_var_u64(&msg, &mut pos), Some(MSG_AWARENESS));
        let payload = read_var_bytes(&msg, &mut pos).unwrap();
        assert_eq!(parse_awareness(payload).unwrap(), entries);
    }

    fn test_room(initial: &str) -> (tempfile::TempDir, Arc<Room>) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().to_path_buf()).unwrap();
        store.create_project("p").unwrap();
        store
            .write_file("p", "main.tex", initial.as_bytes())
            .unwrap();
        (tmp, Room::new(store, "p", "main.tex", initial, None))
    }

    #[test]
    fn client_syncs_and_edits_round_trip() {
        let (_tmp, room) = test_room("hello");
        let mut conn = Conn {
            id: 1,
            ..Conn::default()
        };

        // Client with an empty document asks for the server state.
        let client = Doc::new();
        let ctext = client.get_or_insert_text(TEXT_KEY);
        let sv = client.transact().state_vector().encode_v1();
        let replies = room.handle(&mut conn, &sync_message(SYNC_STEP1, &sv));
        assert_eq!(replies.len(), 1);
        let mut pos = 0;
        assert_eq!(read_var_u64(&replies[0], &mut pos), Some(MSG_SYNC));
        assert_eq!(read_var_u64(&replies[0], &mut pos), Some(SYNC_STEP2));
        let update = read_var_bytes(&replies[0], &mut pos).unwrap();
        client
            .transact_mut()
            .apply_update(Update::decode_v1(update).unwrap())
            .unwrap();
        assert_eq!(ctext.get_string(&client.transact()), "hello");

        // Client edits; the update reaches the server document and is marked dirty.
        let before = client.transact().state_vector();
        ctext.insert(&mut client.transact_mut(), 5, " world");
        let diff = client.transact().encode_state_as_update_v1(&before);
        room.handle(&mut conn, &sync_message(SYNC_UPDATE, &diff));
        assert_eq!(room.text(), "hello world");
        assert!(room.dirty.load(Ordering::SeqCst));

        room.save();
        let on_disk = room.store.read_file("p", "main.tex").unwrap();
        assert_eq!(on_disk, b"hello world");
    }

    #[test]
    fn persisted_state_survives_restart_without_duplication() {
        let (_tmp, room) = test_room("abc");
        let mut conn = Conn {
            id: 1,
            ..Conn::default()
        };

        // A client syncs, then edits.
        let client = Doc::new();
        let ctext = client.get_or_insert_text(TEXT_KEY);
        let full = room
            .doc
            .lock()
            .unwrap()
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        client
            .transact_mut()
            .apply_update(Update::decode_v1(&full).unwrap())
            .unwrap();
        let before = client.transact().state_vector();
        ctext.insert(&mut client.transact_mut(), 3, "def");
        let diff = client.transact().encode_state_as_update_v1(&before);
        room.handle(&mut conn, &sync_message(SYNC_UPDATE, &diff));
        room.save();

        // "Restart": a new room built from what is on disk keeps the same epoch.
        let store = room.store.clone();
        let disk = String::from_utf8(store.read_file("p", "main.tex").unwrap()).unwrap();
        let reborn = Room::new(
            store.clone(),
            "p",
            "main.tex",
            &disk,
            store.load_ydoc("p", "main.tex"),
        );
        assert_eq!(reborn.epoch, room.epoch);

        // The old client reconnects and exchanges state: no duplicated text.
        let mut conn2 = Conn {
            id: 2,
            ..Conn::default()
        };
        let sv = client.transact().state_vector().encode_v1();
        let replies = reborn.handle(&mut conn2, &sync_message(SYNC_STEP1, &sv));
        let mut pos = 0;
        read_var_u64(&replies[0], &mut pos);
        read_var_u64(&replies[0], &mut pos);
        let update = read_var_bytes(&replies[0], &mut pos).unwrap();
        client
            .transact_mut()
            .apply_update(Update::decode_v1(update).unwrap())
            .unwrap();
        assert_eq!(ctext.get_string(&client.transact()), "abcdef");
        assert_eq!(reborn.text(), "abcdef");
    }

    #[test]
    fn external_edit_invalidates_persisted_state() {
        let (_tmp, room) = test_room("one");
        room.dirty.store(true, Ordering::SeqCst);
        room.save();
        let store = room.store.clone();
        let reborn = Room::new(
            store.clone(),
            "p",
            "main.tex",
            "changed on disk",
            store.load_ydoc("p", "main.tex"),
        );
        assert_ne!(reborn.epoch, room.epoch);
        assert_eq!(reborn.text(), "changed on disk");
    }

    #[test]
    fn presence_is_cached_and_cleared() {
        let (_tmp, room) = test_room("");
        let mut conn = Conn {
            id: 9,
            ..Conn::default()
        };
        let msg = awareness_message(&[(42, 1, br#"{"user":{"name":"A"}}"#.to_vec())]);
        room.handle(&mut conn, &msg);
        assert!(room.awareness_snapshot().is_some());
        room.drop_presence(&conn);
        assert!(room.awareness_snapshot().is_none());
    }

    #[test]
    fn garbage_is_ignored() {
        let (_tmp, room) = test_room("x");
        let mut conn = Conn::default();
        assert!(room.handle(&mut conn, &[]).is_empty());
        assert!(room.handle(&mut conn, &[0, 0, 200]).is_empty());
        assert!(room.handle(&mut conn, &[0, 2, 3, 1, 2, 3]).is_empty());
        assert_eq!(room.text(), "x");
    }
}
