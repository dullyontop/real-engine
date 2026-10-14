//! Typed, double-buffered event queues.
//!
//! An event sent during frame N stays readable until the end of frame N + 1, so a
//! reader that runs before the sender in the frame still sees it exactly once.
//! Each reader keeps its own [`EventCursor`]. The owner calls
//! [`Events::update`] once per frame (in `EndFrame`), which drops the events of
//! the older buffer.
//!
//! ```
//! use re_core::{EventCursor, Events};
//!
//! let mut events = Events::new();
//! let mut reader = EventCursor::default();
//! events.send("collected");
//! assert_eq!(reader.read(&events).collect::<Vec<_>>(), [&"collected"]);
//! assert_eq!(reader.read(&events).count(), 0); // each event is read once
//! ```

/// A queue of events of type `T`, kept for two [`update`](Events::update) cycles.
#[derive(Debug, Clone)]
pub struct Events<T> {
    /// Events sent before the last `update`.
    previous: Vec<T>,
    /// The id of `previous[0]`.
    previous_start: u64,
    /// Events sent since the last `update`.
    current: Vec<T>,
    /// The id of `current[0]`.
    current_start: u64,
}

impl<T> Default for Events<T> {
    fn default() -> Self {
        Events {
            previous: Vec::new(),
            previous_start: 0,
            current: Vec::new(),
            current_start: 0,
        }
    }
}

impl<T> Events<T> {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sends an event; it can be read until the second [`update`](Self::update) after now.
    pub fn send(&mut self, event: T) {
        self.current.push(event);
    }

    /// Sends several events in order.
    pub fn send_batch(&mut self, events: impl IntoIterator<Item = T>) {
        self.current.extend(events);
    }

    /// Ends a frame: drops the older buffer and starts a new current one.
    pub fn update(&mut self) {
        let next_start = self.next_id();
        self.previous = std::mem::take(&mut self.current);
        self.previous_start = self.current_start;
        self.current_start = next_start;
    }

    /// Removes and returns every stored event, oldest first. Cursors stay valid:
    /// they simply find nothing new.
    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        let next_start = self.next_id();
        self.previous_start = next_start;
        self.current_start = next_start;
        self.previous.drain(..).chain(self.current.drain(..))
    }

    /// Drops every stored event.
    pub fn clear(&mut self) {
        self.drain().for_each(drop);
    }

    /// Every stored event (both buffers), oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.previous.iter().chain(self.current.iter())
    }

    /// The events sent since the last [`update`](Self::update).
    pub fn iter_current(&self) -> impl Iterator<Item = &T> {
        self.current.iter()
    }

    /// The number of stored events.
    pub fn len(&self) -> usize {
        self.previous.len() + self.current.len()
    }

    /// True when no event is stored.
    pub fn is_empty(&self) -> bool {
        self.previous.is_empty() && self.current.is_empty()
    }

    /// A cursor that will read only events sent from now on.
    pub fn cursor_at_end(&self) -> EventCursor {
        EventCursor {
            next: self.next_id(),
        }
    }

    fn next_id(&self) -> u64 {
        self.current_start + self.current.len() as u64
    }
}

/// A reader's position in an [`Events`] queue.
///
/// The default cursor starts at the beginning, so it reads every stored event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventCursor {
    next: u64,
}

impl EventCursor {
    /// Returns the events this cursor has not read yet, oldest first, and marks
    /// them read. Events dropped by `update` before being read are skipped.
    pub fn read<'a, T>(
        &mut self,
        events: &'a Events<T>,
    ) -> impl Iterator<Item = &'a T> + use<'a, T> {
        let from = self.next;
        self.next = events.next_id();
        let skip_previous = from.saturating_sub(events.previous_start);
        let skip_current = from.saturating_sub(events.current_start);
        events
            .previous
            .iter()
            .skip(usize::try_from(skip_previous).unwrap_or(usize::MAX))
            .chain(
                events
                    .current
                    .iter()
                    .skip(usize::try_from(skip_current).unwrap_or(usize::MAX)),
            )
    }

    /// The number of events this cursor has not read yet.
    pub fn unread<T>(&self, events: &Events<T>) -> usize {
        let oldest = events.previous_start;
        let end = events.next_id();
        usize::try_from(end - self.next.clamp(oldest, end)).unwrap_or(usize::MAX)
    }
}
