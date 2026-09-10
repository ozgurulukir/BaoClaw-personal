//! Inbound JSON-RPC message routing, shared by every transport's pump.
//! Pure decision logic: given the pending-request map and the live-catalog
//! signal, decide where a message goes and what (if anything) to write back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use tokio::sync::{oneshot, watch};

use crate::ipc::protocol::{JsonRpcErrorResponse, JsonRpcMessage, JsonRpcResponse, RequestId};

/// The client's in-flight request demux. std Mutex: never held across an
/// await.
pub(crate) type PendingMap = std::sync::Mutex<HashMap<RequestId, oneshot::Sender<JsonRpcMessage>>>;

/// Monotonic counter bumped on every `notifications/tools/list_changed`;
/// the supervisor waits for the watch value to move past the value it last
/// observed.
#[derive(Default)]
pub(crate) struct ListChangedSignal {
    counter: AtomicU64,
}

impl ListChangedSignal {
    pub fn bump(&self, tx: &watch::Sender<u64>) {
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        tx.send_modify(|v| *v = n);
    }
}

/// Route one decoded inbound message.
///
/// Returns `Some(reply)` when the client must write a message back
/// (server→client requests: `ping` is answered with `{}`, everything else
/// gets method-not-found so the server can degrade). The caller owns the
/// write deadline; the connection is dead when that write cannot complete
/// promptly.
///
/// `Ok` responses and error responses WITH an id complete the matching
/// pending entry; late responses find no sender (already removed on
/// timeout/cancel) and are dropped — ids are monotonic, so nothing can be
/// misdelivered to a later request.
pub(crate) fn route_inbound(
    pending: &PendingMap,
    list_changed_tx: &watch::Sender<u64>,
    list_changed_signal: &ListChangedSignal,
    msg: JsonRpcMessage,
) -> Option<JsonRpcMessage> {
    match msg {
        JsonRpcMessage::Response(resp) => {
            let tx = pending.lock().unwrap().remove(&resp.id);
            if let Some(tx) = tx {
                let _ = tx.send(JsonRpcMessage::Response(resp));
            }
            None
        }
        JsonRpcMessage::ErrorResponse(err) => {
            if let Some(id) = &err.id {
                let tx = pending.lock().unwrap().remove(id);
                if let Some(tx) = tx {
                    let _ = tx.send(JsonRpcMessage::ErrorResponse(err));
                }
            }
            None
        }
        JsonRpcMessage::Request(req) => {
            // Server→client requests are out of scope: ping is answered,
            // everything else (sampling, roots, ...) gets method-not-found.
            if req.method == "ping" {
                Some(JsonRpcMessage::Response(JsonRpcResponse::success(
                    req.id.clone(),
                    json!({}),
                )))
            } else {
                Some(JsonRpcMessage::ErrorResponse(JsonRpcErrorResponse::new(
                    Some(req.id.clone()),
                    -32601,
                    "server-to-client requests are not supported".into(),
                )))
            }
        }
        JsonRpcMessage::Notification(n) => {
            if n.method == "notifications/tools/list_changed" {
                list_changed_signal.bump(list_changed_tx);
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture {
        pending: PendingMap,
        list_tx: watch::Sender<u64>,
        list_rx: watch::Receiver<u64>,
        signal: ListChangedSignal,
    }

    fn setup() -> Fixture {
        let (list_tx, list_rx) = watch::channel(0u64);
        let signal = ListChangedSignal::default();
        Fixture {
            pending: std::sync::Mutex::new(HashMap::new()),
            list_tx,
            list_rx,
            signal,
        }
    }

    fn route(fx: &Fixture, msg: JsonRpcMessage) -> Option<JsonRpcMessage> {
        route_inbound(&fx.pending, &fx.list_tx, &fx.signal, msg)
    }

    #[test]
    fn response_completes_pending_entry() {
        let fx = setup();
        let (stx, mut srx) = oneshot::channel();
        fx.pending.lock().unwrap().insert(RequestId::Number(7), stx);
        let msg = JsonRpcMessage::Response(crate::ipc::protocol::JsonRpcResponse::success(
            RequestId::Number(7),
            json!({"ok": true}),
        ));
        assert!(route(&fx, msg).is_none());
        assert!(srx.try_recv().is_ok(), "waiter must be released");
        assert!(fx.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn late_response_with_removed_entry_is_dropped() {
        let fx = setup();
        let msg = JsonRpcMessage::Response(crate::ipc::protocol::JsonRpcResponse::success(
            RequestId::Number(9),
            json!(null),
        ));
        assert!(route(&fx, msg).is_none());
    }

    #[test]
    fn ping_is_answered_other_requests_rejected() {
        let fx = setup();
        let ping = JsonRpcMessage::Request(crate::ipc::protocol::JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "ping".into(),
            params: json!({}),
            id: RequestId::Number(1),
        });
        match route(&fx, ping) {
            Some(JsonRpcMessage::Response(r)) => assert_eq!(r.result, json!({})),
            other => panic!("expected ping reply, got {other:?}"),
        }
        let sampling = JsonRpcMessage::Request(crate::ipc::protocol::JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "sampling/createMessage".into(),
            params: json!({}),
            id: RequestId::Number(2),
        });
        match route(&fx, sampling) {
            Some(JsonRpcMessage::ErrorResponse(e)) => assert_eq!(e.error.code, -32601),
            other => panic!("expected -32601, got {other:?}"),
        }
    }

    #[test]
    fn list_changed_notification_bumps_counter() {
        let fx = setup();
        let notif = || {
            JsonRpcMessage::Notification(crate::ipc::protocol::JsonRpcNotification::new(
                "notifications/tools/list_changed",
                json!({}),
            ))
        };
        route(&fx, notif());
        assert_eq!(*fx.list_rx.borrow(), 1);
        route(&fx, notif());
        assert_eq!(*fx.list_rx.borrow(), 2);
    }

    #[test]
    fn unrelated_notification_is_ignored() {
        let fx = setup();
        route(
            &fx,
            JsonRpcMessage::Notification(crate::ipc::protocol::JsonRpcNotification::new(
                "other",
                json!({}),
            )),
        );
        assert_eq!(*fx.list_rx.borrow(), 0);
    }
}
