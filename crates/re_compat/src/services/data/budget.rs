//! Per-server request budgets of data stores (ADR-0048): one bucket per
//! request type that refills on simulation time, with a queue of 30 requests
//! behind it.

/// How many requests may wait behind an empty budget, per request type.
pub const QUEUE_LIMIT: f64 = 30.0;

/// The request types budgets are kept for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `GetAsync` and the version reads of a standard store.
    StandardRead,
    /// `SetAsync` and `IncrementAsync` of a standard store.
    StandardWrite,
    /// `ListKeysAsync`, `ListVersionsAsync` and `ListDataStoresAsync`.
    StandardList,
    /// `RemoveAsync` of a standard store.
    StandardRemove,
    /// `GetAsync` of an ordered store.
    OrderedRead,
    /// `SetAsync` and `IncrementAsync` of an ordered store.
    OrderedWrite,
    /// `GetSortedAsync`.
    OrderedList,
    /// `RemoveAsync` of an ordered store.
    OrderedRemove,
}

impl Kind {
    /// Every kind, in bucket order.
    pub const ALL: [Kind; 8] = [
        Kind::StandardRead,
        Kind::StandardWrite,
        Kind::StandardList,
        Kind::StandardRemove,
        Kind::OrderedRead,
        Kind::OrderedWrite,
        Kind::OrderedList,
        Kind::OrderedRemove,
    ];

    /// Requests per minute with `players` players in the server.
    pub fn per_minute(self, players: u32) -> f64 {
        let p = f64::from(players);
        match self {
            Kind::StandardRead | Kind::StandardWrite | Kind::StandardRemove | Kind::OrderedRead => {
                60.0 + 40.0 * p
            }
            Kind::OrderedWrite | Kind::OrderedRemove => 30.0 + 5.0 * p,
            Kind::StandardList | Kind::OrderedList => 5.0 + 2.0 * p,
        }
    }

    fn index(self) -> usize {
        Kind::ALL.iter().position(|k| *k == self).expect("listed")
    }
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    /// Requests available; negative while requests wait in the queue.
    tokens: f64,
    /// When `tokens` was last brought up to date (simulation seconds).
    at: f64,
    started: bool,
}

/// The budgets of one server.
#[derive(Clone, Debug)]
pub struct Budgets {
    buckets: [Bucket; 8],
}

impl Default for Budgets {
    fn default() -> Budgets {
        Budgets {
            buckets: [Bucket {
                tokens: 0.0,
                at: 0.0,
                started: false,
            }; 8],
        }
    }
}

impl Budgets {
    fn refill(&mut self, kind: Kind, now: f64, players: u32) -> &mut Bucket {
        let cap = kind.per_minute(players);
        let b = &mut self.buckets[kind.index()];
        if !b.started {
            // A server starts with a full minute of budget.
            *b = Bucket {
                tokens: cap,
                at: now,
                started: true,
            };
        } else if now > b.at {
            b.tokens = (b.tokens + (now - b.at) * cap / 60.0).min(cap);
            b.at = now;
        }
        b
    }

    /// The whole requests of `kind` that would run at once.
    pub fn available(&mut self, kind: Kind, now: f64, players: u32) -> f64 {
        self.refill(kind, now, players).tokens.max(0.0).floor()
    }

    /// Takes one request from each of `kinds` (two for `UpdateAsync`).
    /// Returns how many seconds the request waits for its budget, or the kind
    /// whose queue is full, in which case nothing is taken.
    pub fn reserve(&mut self, kinds: &[Kind], now: f64, players: u32) -> Result<f64, Kind> {
        for &k in kinds {
            if self.refill(k, now, players).tokens - 1.0 < -QUEUE_LIMIT {
                return Err(k);
            }
        }
        let mut wait: f64 = 0.0;
        for &k in kinds {
            let rate = k.per_minute(players) / 60.0;
            let b = self.refill(k, now, players);
            b.tokens -= 1.0;
            if b.tokens < 0.0 {
                wait = wait.max(-b.tokens / rate);
            }
        }
        Ok(wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_follow_the_player_count() {
        assert_eq!(Kind::StandardRead.per_minute(0), 60.0);
        assert_eq!(Kind::StandardWrite.per_minute(2), 140.0);
        assert_eq!(Kind::OrderedWrite.per_minute(4), 50.0);
        assert_eq!(Kind::OrderedList.per_minute(10), 25.0);
    }

    #[test]
    fn an_empty_budget_queues_thirty_then_refuses() {
        let mut b = Budgets::default();
        // 5 + 2×0 list requests a minute: 5 at once, then 30 queued.
        for _ in 0..5 {
            assert_eq!(b.reserve(&[Kind::StandardList], 0.0, 0), Ok(0.0));
        }
        let first = b.reserve(&[Kind::StandardList], 0.0, 0).unwrap();
        assert!((first - 12.0).abs() < 1e-9, "{first}");
        for _ in 1..30 {
            b.reserve(&[Kind::StandardList], 0.0, 0).unwrap();
        }
        assert_eq!(
            b.reserve(&[Kind::StandardList], 0.0, 0),
            Err(Kind::StandardList)
        );
        assert_eq!(b.available(Kind::StandardList, 0.0, 0), 0.0);
        // A minute later six requests' worth came back: the queue has room.
        assert!(b.reserve(&[Kind::StandardList], 60.0, 0).is_ok());
    }

    #[test]
    fn refills_stop_at_one_minute_of_budget() {
        let mut b = Budgets::default();
        b.reserve(&[Kind::StandardRead], 0.0, 0).unwrap();
        assert_eq!(b.available(Kind::StandardRead, 0.0, 0), 59.0);
        assert_eq!(b.available(Kind::StandardRead, 600.0, 0), 60.0);
    }

    #[test]
    fn update_charges_both_budgets_or_neither() {
        let mut b = Budgets::default();
        for _ in 0..(30 + 30) {
            b.reserve(&[Kind::OrderedWrite], 0.0, 0).unwrap();
        }
        let before = b.available(Kind::OrderedRead, 0.0, 0);
        assert_eq!(
            b.reserve(&[Kind::OrderedRead, Kind::OrderedWrite], 0.0, 0),
            Err(Kind::OrderedWrite)
        );
        assert_eq!(b.available(Kind::OrderedRead, 0.0, 0), before);
    }
}
