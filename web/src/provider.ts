// Yjs WebSocket provider speaking the y-websocket wire protocol, plus a custom
// "epoch" message so clients notice when the server's copy of a document has
// been replaced (for instance after the file changed on disk).

import * as decoding from "lib0/decoding";
import * as encoding from "lib0/encoding";
import * as awarenessProtocol from "y-protocols/awareness";
import * as syncProtocol from "y-protocols/sync";
import * as Y from "yjs";

const MSG_SYNC = 0;
const MSG_AWARENESS = 1;
const MSG_EPOCH = 100;

export type ConnectionStatus = "connecting" | "connected" | "disconnected";

export interface ProviderHandlers {
  onStatus?: (status: ConnectionStatus) => void;
  /** The initial state exchange with the server finished. */
  onSynced?: () => void;
  /** The server's document was replaced; the caller must start over with a fresh Y.Doc. */
  onReset?: () => void;
}

export class DocProvider {
  readonly doc: Y.Doc;
  readonly awareness: awarenessProtocol.Awareness;
  status: ConnectionStatus = "connecting";
  synced = false;

  private ws: WebSocket | null = null;
  private epoch: string | null = null;
  private attempts = 0;
  private timer: number | undefined;
  private destroyed = false;

  constructor(
    private readonly url: string,
    private readonly params: Record<string, string>,
    private readonly handlers: ProviderHandlers = {},
  ) {
    this.doc = new Y.Doc();
    this.awareness = new awarenessProtocol.Awareness(this.doc);
    this.doc.on("update", this.onDocUpdate);
    this.awareness.on("update", this.onAwarenessUpdate);
    window.addEventListener("beforeunload", this.onUnload);
    this.connect();
  }

  private setStatus(status: ConnectionStatus) {
    if (this.status === status) return;
    this.status = status;
    this.handlers.onStatus?.(status);
  }

  private connect() {
    if (this.destroyed) return;
    this.setStatus("connecting");
    const query = new URLSearchParams(this.params).toString();
    const ws = new WebSocket(`${this.url}?${query}`);
    ws.binaryType = "arraybuffer";
    this.ws = ws;

    ws.onopen = () => {
      this.attempts = 0;
      this.setStatus("connected");
      const enc = encoding.createEncoder();
      encoding.writeVarUint(enc, MSG_SYNC);
      syncProtocol.writeSyncStep1(enc, this.doc);
      ws.send(encoding.toUint8Array(enc));
      if (this.awareness.getLocalState() !== null) {
        this.send(this.awarenessMessage([this.doc.clientID]));
      }
    };

    ws.onmessage = (event) => {
      if (!(event.data instanceof ArrayBuffer)) return;
      this.handleMessage(new Uint8Array(event.data));
    };

    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.synced = false;
      this.setStatus("disconnected");
      const others = [...this.awareness.getStates().keys()].filter((id) => id !== this.doc.clientID);
      awarenessProtocol.removeAwarenessStates(this.awareness, others, this);
      if (this.destroyed) return;
      this.attempts += 1;
      const delay = Math.min(5000, 250 * 2 ** Math.min(this.attempts, 5));
      this.timer = window.setTimeout(() => this.connect(), delay);
    };

    ws.onerror = () => ws.close();
  }

  private handleMessage(data: Uint8Array) {
    const dec = decoding.createDecoder(data);
    const type = decoding.readVarUint(dec);
    switch (type) {
      case MSG_SYNC: {
        const enc = encoding.createEncoder();
        encoding.writeVarUint(enc, MSG_SYNC);
        const step = syncProtocol.readSyncMessage(dec, enc, this.doc, this);
        if (encoding.length(enc) > 1) this.send(encoding.toUint8Array(enc));
        if (step === syncProtocol.messageYjsSyncStep2 && !this.synced) {
          this.synced = true;
          this.handlers.onSynced?.();
        }
        break;
      }
      case MSG_AWARENESS:
        awarenessProtocol.applyAwarenessUpdate(this.awareness, decoding.readVarUint8Array(dec), this);
        break;
      case MSG_EPOCH: {
        const epoch = decoding.readVarString(dec);
        if (this.epoch !== null && this.epoch !== epoch) {
          this.handlers.onReset?.();
        }
        this.epoch = epoch;
        break;
      }
      default:
        break;
    }
  }

  private send(data: Uint8Array) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(data as Uint8Array<ArrayBuffer>);
  }

  private awarenessMessage(clients: number[]): Uint8Array {
    const enc = encoding.createEncoder();
    encoding.writeVarUint(enc, MSG_AWARENESS);
    encoding.writeVarUint8Array(enc, awarenessProtocol.encodeAwarenessUpdate(this.awareness, clients));
    return encoding.toUint8Array(enc);
  }

  private onDocUpdate = (update: Uint8Array, origin: unknown) => {
    if (origin === this) return;
    const enc = encoding.createEncoder();
    encoding.writeVarUint(enc, MSG_SYNC);
    syncProtocol.writeUpdate(enc, update);
    this.send(encoding.toUint8Array(enc));
  };

  private onAwarenessUpdate = (
    change: { added: number[]; updated: number[]; removed: number[] },
    origin: unknown,
  ) => {
    if (origin === this) return;
    this.send(this.awarenessMessage([...change.added, ...change.updated, ...change.removed]));
  };

  private onUnload = () => {
    awarenessProtocol.removeAwarenessStates(this.awareness, [this.doc.clientID], "unload");
  };

  destroy() {
    if (this.destroyed) return;
    awarenessProtocol.removeAwarenessStates(this.awareness, [this.doc.clientID], "destroy");
    this.destroyed = true;
    window.removeEventListener("beforeunload", this.onUnload);
    window.clearTimeout(this.timer);
    this.doc.off("update", this.onDocUpdate);
    this.awareness.off("update", this.onAwarenessUpdate);
    this.ws?.close();
    this.ws = null;
    this.awareness.destroy();
    this.doc.destroy();
  }
}
