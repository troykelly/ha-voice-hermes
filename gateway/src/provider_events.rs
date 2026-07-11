use std::cell::{Cell, RefCell};
use std::rc::Rc;

use futures_channel::mpsc::{channel, Receiver, Sender};
use futures_util::StreamExt;
use wasm_bindgen::{closure::Closure, JsCast};
use worker::{web_sys, Error, Result, WebSocket};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventFailure {
    InvalidMessage,
    QueueOverflow,
    SocketError,
}

#[derive(Debug)]
pub enum ProviderEvent {
    Text(String),
    Close,
    Error,
}

/// A bounded event adapter for an outbound provider WebSocket.
///
/// workers-rs' `WebSocket::events()` uses an unbounded MPSC channel. That is
/// unsafe for audio providers because device backpressure can pause the
/// consumer while the provider continues emitting messages. This adapter
/// validates text messages before enqueueing them, keeps only a fixed number
/// of events, and closes the provider socket on overflow.
pub struct BoundedWebSocketEvents {
    socket: WebSocket,
    receiver: Receiver<ProviderEvent>,
    failure: Rc<Cell<Option<EventFailure>>>,
    message_handler: Option<Closure<dyn FnMut(web_sys::MessageEvent)>>,
    error_handler: Option<Closure<dyn FnMut(web_sys::ErrorEvent)>>,
    close_handler: Option<Closure<dyn FnMut(web_sys::CloseEvent)>>,
}

impl BoundedWebSocketEvents {
    pub fn new(socket: &WebSocket, max_message_bytes: usize, capacity: usize) -> Result<Self> {
        if max_message_bytes == 0 || capacity == 0 {
            return Err(Error::RustError(
                "provider event bounds must be non-zero".into(),
            ));
        }

        let (sender, receiver) = channel(capacity);
        let sender = Rc::new(RefCell::new(sender));
        let failure = Rc::new(Cell::new(None));

        let message_handler = Closure::wrap(Box::new({
            let sender = sender.clone();
            let failure = failure.clone();
            let socket = socket.clone();
            move |event: web_sys::MessageEvent| {
                let Some(text) = event.data().as_string() else {
                    signal_failure(&sender, &failure, &socket, EventFailure::InvalidMessage);
                    return;
                };
                if text.len() > max_message_bytes {
                    signal_failure(&sender, &failure, &socket, EventFailure::InvalidMessage);
                    return;
                }
                enqueue(&sender, &failure, &socket, ProviderEvent::Text(text));
            }
        }) as Box<dyn FnMut(web_sys::MessageEvent)>);

        let error_handler = Closure::wrap(Box::new({
            let sender = sender.clone();
            let failure = failure.clone();
            let socket = socket.clone();
            move |_event: web_sys::ErrorEvent| {
                signal_failure(&sender, &failure, &socket, EventFailure::SocketError);
            }
        }) as Box<dyn FnMut(web_sys::ErrorEvent)>);

        let close_handler = Closure::wrap(Box::new({
            let sender = sender.clone();
            let failure = failure.clone();
            let socket = socket.clone();
            move |_event: web_sys::CloseEvent| {
                enqueue(&sender, &failure, &socket, ProviderEvent::Close);
            }
        }) as Box<dyn FnMut(web_sys::CloseEvent)>);

        let target = socket.as_ref();
        target
            .add_event_listener_with_callback("message", message_handler.as_ref().unchecked_ref())
            .map_err(Error::from)?;
        if let Err(error) =
            target.add_event_listener_with_callback("error", error_handler.as_ref().unchecked_ref())
        {
            let _ = target.remove_event_listener_with_callback(
                "message",
                message_handler.as_ref().unchecked_ref(),
            );
            return Err(Error::from(error));
        }
        if let Err(error) =
            target.add_event_listener_with_callback("close", close_handler.as_ref().unchecked_ref())
        {
            let _ = target.remove_event_listener_with_callback(
                "message",
                message_handler.as_ref().unchecked_ref(),
            );
            let _ = target.remove_event_listener_with_callback(
                "error",
                error_handler.as_ref().unchecked_ref(),
            );
            return Err(Error::from(error));
        }

        Ok(Self {
            socket: socket.clone(),
            receiver,
            failure,
            message_handler: Some(message_handler),
            error_handler: Some(error_handler),
            close_handler: Some(close_handler),
        })
    }

    pub async fn next(&mut self) -> Result<Option<ProviderEvent>> {
        if let Some(failure) = self.failure.get() {
            return Err(failure_error(failure));
        }
        match self.receiver.next().await {
            Some(ProviderEvent::Error) => Err(failure_error(
                self.failure.get().unwrap_or(EventFailure::SocketError),
            )),
            event => Ok(event),
        }
    }
}

impl Drop for BoundedWebSocketEvents {
    fn drop(&mut self) {
        let target = self.socket.as_ref();
        if let Some(handler) = self.message_handler.take() {
            let _ = target
                .remove_event_listener_with_callback("message", handler.as_ref().unchecked_ref());
        }
        if let Some(handler) = self.error_handler.take() {
            let _ = target
                .remove_event_listener_with_callback("error", handler.as_ref().unchecked_ref());
        }
        if let Some(handler) = self.close_handler.take() {
            let _ = target
                .remove_event_listener_with_callback("close", handler.as_ref().unchecked_ref());
        }
    }
}

fn enqueue(
    sender: &Rc<RefCell<Sender<ProviderEvent>>>,
    failure: &Rc<Cell<Option<EventFailure>>>,
    socket: &WebSocket,
    event: ProviderEvent,
) {
    let result = sender.borrow_mut().try_send(event);
    if let Err(error) = result {
        if error.is_full() {
            failure.set(Some(EventFailure::QueueOverflow));
            let _ = socket.close(Some(1011), Some("provider event queue overflow"));
        }
    }
}

fn signal_failure(
    sender: &Rc<RefCell<Sender<ProviderEvent>>>,
    failure: &Rc<Cell<Option<EventFailure>>>,
    socket: &WebSocket,
    reason: EventFailure,
) {
    failure.set(Some(reason));
    let _ = sender.borrow_mut().try_send(ProviderEvent::Error);
    let _ = socket.close(Some(1003), Some("invalid provider websocket event"));
}

fn failure_error(failure: EventFailure) -> Error {
    let message = match failure {
        EventFailure::InvalidMessage => "provider returned an invalid websocket message",
        EventFailure::QueueOverflow => "provider websocket event queue overflowed",
        EventFailure::SocketError => "provider websocket reported an error",
    };
    Error::RustError(message.into())
}
