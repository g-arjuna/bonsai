/**
 * EV1-6 T9: SseManager — single EventSource connection to /api/ml/events/stream.
 * Dispatches events to per-type Svelte stores. Auto-reconnects with backoff.
 */
import { writable } from 'svelte/store';

export const sseConnected = writable(false);
export const lastJobEvent = writable(null);
export const lastGnnEvent = writable(null);
export const lastEmbeddingEvent = writable(null);
export const lastProgressEvent = writable(null);
export const lastDetectionEvent = writable(null);

const BACKOFF = [1000, 2000, 4000, 8000, 16000, 30000];
const HEARTBEAT_TIMEOUT = 60000;

let es = null;
let backoffIdx = 0;
let heartbeatTimer = null;
let reconnectTimer = null;

const storeByType = {
  job_started:     lastJobEvent,
  job_completed:   lastJobEvent,
  job_failed:      lastJobEvent,
  job_dead_letter: lastJobEvent,
  job_progress:    lastProgressEvent,
  gnn_anomaly:     lastGnnEvent,
  gnn_inference_complete: lastGnnEvent,
  embedding_batch: lastEmbeddingEvent,
  detection_fired: lastDetectionEvent,
};

function resetHeartbeat() {
  clearTimeout(heartbeatTimer);
  heartbeatTimer = setTimeout(() => {
    console.warn('[SSE] heartbeat timeout — reconnecting');
    reconnect();
  }, HEARTBEAT_TIMEOUT);
}

function reconnect() {
  if (es) { es.close(); es = null; }
  clearTimeout(reconnectTimer);
  const delay = BACKOFF[Math.min(backoffIdx, BACKOFF.length - 1)];
  backoffIdx++;
  reconnectTimer = setTimeout(connect, delay);
}

export function connect() {
  if (es) return;
  es = new EventSource('/api/ml/events/stream');

  es.onopen = () => {
    sseConnected.set(true);
    backoffIdx = 0;
    resetHeartbeat();
  };

  es.onmessage = (e) => {
    resetHeartbeat();
    try {
      const msg = JSON.parse(e.data);
      const store = storeByType[msg.event_type];
      if (store) store.set(msg);
    } catch (_) {}
  };

  es.onerror = () => {
    sseConnected.set(false);
    reconnect();
  };
}

export function disconnect() {
  clearTimeout(heartbeatTimer);
  clearTimeout(reconnectTimer);
  if (es) { es.close(); es = null; }
  sseConnected.set(false);
}
