//! The browser's WebSocket as a client transport (ADR-0049 §10).
//!
//! The page's `WebSocket` delivers messages and its close as JavaScript
//! events, between frames: the handlers put them where the next
//! [`Transport::advance`] and receive find them. A frame never waits for one,
//! so a driver that needs a message (a joining or lockstep client) returns to
//! the page's event loop until it came.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

use re_core::Diagnostic;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, CloseEvent, MessageEvent, WebSocket};

use super::{MAX_INBOX, MAX_QUEUED, MAX_WRITE_BUFFER};
use crate::codes;
use crate::transport::Transport;

/// What the socket's handlers leave for the transport.
#[derive(Default)]
struct Events {
    inbox: VecDeque<Vec<u8>>,
    opened: bool,
    /// Why the socket closed, once it did.
    closed: Option<Diagnostic>,
}

/// A client's WebSocket to its server, in a browser. The endpoint reaches
/// the server at [`WsTransport::server_addr`].
///
/// Datagrams sent while the socket opens wait for it (at most 64); one sent
/// while 1 MiB is still buffered is lost, as on a congested network.
pub struct WsTransport {
    url: String,
    socket: WebSocket,
    events: Rc<RefCell<Events>>,
    queued: VecDeque<Vec<u8>>,
    reported: bool,
    // Kept alive while the socket may call them; unset on drop.
    _on_open: Closure<dyn FnMut(wasm_bindgen::JsValue)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
}

impl std::fmt::Debug for WsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsTransport")
            .field("url", &self.url)
            .field("ready_state", &self.socket.ready_state())
            .finish_non_exhaustive()
    }
}

impl WsTransport {
    /// Opens a WebSocket to `url` (`ws://` or `wss://`). It connects between
    /// frames; a failure arrives as a lost link (`RE8910`).
    ///
    /// # Errors
    ///
    /// `RE8910` when the browser refuses the address at once: it does not
    /// parse, or the page is served over HTTPS and the address is `ws://`.
    pub fn connect(url: &str, _timeout: Duration) -> Result<WsTransport, Diagnostic> {
        let url = url.trim().to_string();
        let socket = WebSocket::new(&url).map_err(|e| {
            Diagnostic::error(
                codes::WEBSOCKET,
                format!(
                    "the browser cannot open a WebSocket to {url}: {}",
                    describe_js(&e)
                ),
            )
            .with_hint(
                "write the address as ws://host:port/; a page served over HTTPS needs a wss:// address (a TLS proxy in front of the server)",
            )
        })?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let events = Rc::new(RefCell::new(Events::default()));

        let on_open = {
            let events = events.clone();
            Closure::<dyn FnMut(wasm_bindgen::JsValue)>::new(move |_| {
                events.borrow_mut().opened = true;
            })
        };
        let on_message = {
            let events = events.clone();
            let url = url.clone();
            Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
                let mut ev = events.borrow_mut();
                if ev.closed.is_some() {
                    return;
                }
                match e.data().dyn_into::<js_sys::ArrayBuffer>() {
                    Ok(buf) => {
                        if ev.inbox.len() < MAX_INBOX {
                            ev.inbox.push_back(js_sys::Uint8Array::new(&buf).to_vec());
                        }
                    }
                    Err(_) => {
                        ev.closed = Some(Diagnostic::error(
                            codes::WEBSOCKET,
                            format!(
                                "the server at {url} sent a text message, and only binary messages carry datagrams"
                            ),
                        ));
                    }
                }
            })
        };
        let on_close = {
            let events = events.clone();
            let url = url.clone();
            Closure::<dyn FnMut(CloseEvent)>::new(move |e: CloseEvent| {
                let mut ev = events.borrow_mut();
                if ev.closed.is_some() {
                    return;
                }
                let reason = e.reason();
                let detail = if reason.is_empty() {
                    format!("code {}", e.code())
                } else {
                    format!("code {}: {reason}", e.code())
                };
                ev.closed = Some(if !ev.opened {
                    Diagnostic::error(
                        codes::WEBSOCKET,
                        format!("cannot connect to {url} ({detail})"),
                    )
                    .with_hint(
                        "check that the server is running and takes WebSocket players on that TCP port; a page served over HTTPS needs a wss:// address",
                    )
                } else if e.was_clean() {
                    Diagnostic::error(
                        codes::CLOSED,
                        format!("the server at {url} closed the connection ({detail})"),
                    )
                } else {
                    Diagnostic::error(
                        codes::WEBSOCKET,
                        format!("the WebSocket to {url} broke ({detail})"),
                    )
                });
            })
        };
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        Ok(WsTransport {
            url,
            socket,
            events,
            queued: VecDeque::new(),
            reported: false,
            _on_open: on_open,
            _on_message: on_message,
            _on_close: on_close,
        })
    }

    /// The address the endpoint reaches the server at.
    pub fn server_addr(&self) -> SocketAddr {
        super::server_addr()
    }

    fn send_now(&self, datagram: &[u8]) -> io::Result<()> {
        if self.socket.buffered_amount() as usize > MAX_WRITE_BUFFER {
            return Err(io::Error::new(
                ErrorKind::WouldBlock,
                "the WebSocket has 1 MiB waiting to be sent",
            ));
        }
        self.socket
            .send_with_u8_array(datagram)
            .map_err(|e| io::Error::other(describe_js(&e)))
    }
}

/// A JavaScript error for a message.
fn describe_js(e: &wasm_bindgen::JsValue) -> String {
    e.dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}

impl Transport for WsTransport {
    fn send_to(&mut self, to: SocketAddr, datagram: &[u8]) -> io::Result<()> {
        if to != super::server_addr() {
            return Err(ErrorKind::NotConnected.into());
        }
        match self.socket.ready_state() {
            WebSocket::OPEN => {
                self.flush_queued();
                self.send_now(datagram)
            }
            WebSocket::CONNECTING => {
                if self.queued.len() < MAX_QUEUED {
                    self.queued.push_back(datagram.to_vec());
                }
                Ok(())
            }
            _ => Err(ErrorKind::NotConnected.into()),
        }
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        let Some(d) = self.events.borrow_mut().inbox.pop_front() else {
            return Ok(None);
        };
        let n = d.len().min(buf.len());
        buf[..n].copy_from_slice(&d[..n]);
        Ok(Some((n, super::server_addr())))
    }

    fn local_addr(&self) -> SocketAddr {
        super::client_addr()
    }

    fn advance(&mut self, _now: Duration) {
        if self.socket.ready_state() == WebSocket::OPEN {
            self.flush_queued();
        }
    }

    fn take_lost(&mut self) -> Vec<(SocketAddr, Diagnostic)> {
        let events = self.events.borrow();
        // Not before the endpoint read what came first (a Disconnect says why).
        if self.reported || !events.inbox.is_empty() {
            return Vec::new();
        }
        let Some(reason) = events.closed.clone() else {
            return Vec::new();
        };
        drop(events);
        self.reported = true;
        self.queued.clear();
        vec![(super::server_addr(), reason)]
    }

    fn release(&mut self, peer: SocketAddr) {
        if peer == super::server_addr() {
            self.queued.clear();
            self.events.borrow_mut().inbox.clear();
            self.reported = true;
            let _ = self.socket.close_with_code(1000);
        }
    }

    fn describe_peer(&self, peer: SocketAddr) -> String {
        if peer == super::server_addr() {
            self.url.clone()
        } else {
            peer.to_string()
        }
    }
}

impl WsTransport {
    fn flush_queued(&mut self) {
        while let Some(d) = self.queued.pop_front() {
            if self.send_now(&d).is_err() {
                break;
            }
        }
    }
}

impl Drop for WsTransport {
    fn drop(&mut self) {
        // Unset first: the socket must not call a dropped closure.
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close_with_code(1000);
    }
}
