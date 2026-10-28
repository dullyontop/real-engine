//! The replay window of sealed packets (ADR-0067 §5): the highest packet
//! number that opened, and which of the [`WINDOW`] numbers up to it have,
//! kept as RFC 6479 does, in a ring of 64-bit words so moving the window
//! never shifts bits.
//!
//! A receiver asks [`ReplayWindow::fresh`] before it decrypts and calls
//! [`ReplayWindow::mark`] only once the packet opened, so a packet that fails
//! its tag never moves the window.

/// How many packet numbers, up to the highest that opened, the window
/// remembers. A number this far below the highest or more is dropped as too
/// old: 1,024 packets are more than 12 s of a player's traffic.
pub(crate) const WINDOW: u64 = 1024;

/// Words in the ring: one more than the window needs, because the word of the
/// highest number is partly ahead of it.
const WORDS: usize = (WINDOW / 64) as usize + 1;

#[derive(Debug)]
pub(crate) struct ReplayWindow {
    /// The highest packet number that opened, once one has.
    top: Option<u64>,
    /// Bit `n % 64` of word `n / 64 % WORDS`: number `n` opened.
    bits: [u64; WORDS],
}

impl ReplayWindow {
    pub(crate) fn new() -> ReplayWindow {
        ReplayWindow {
            top: None,
            bits: [0; WORDS],
        }
    }

    fn slot(n: u64) -> (usize, u64) {
        ((n / 64 % WORDS as u64) as usize, 1 << (n % 64))
    }

    /// True when a packet numbered `n` may be new: above every number that
    /// opened, or less than [`WINDOW`] below the highest and not opened yet.
    pub(crate) fn fresh(&self, n: u64) -> bool {
        let Some(top) = self.top else {
            return true;
        };
        if n > top {
            return true;
        }
        let (word, bit) = Self::slot(n);
        top - n < WINDOW && self.bits[word] & bit == 0
    }

    /// Records that the packet numbered `n` opened. `n` was [`fresh`].
    ///
    /// [`fresh`]: ReplayWindow::fresh
    pub(crate) fn mark(&mut self, n: u64) {
        debug_assert!(self.fresh(n));
        match self.top {
            Some(top) if n <= top => {}
            Some(top) => {
                // Clear the words the window moves onto: they last held
                // numbers WORDS words older.
                let (from, to) = (top / 64, n / 64);
                if to - from >= WORDS as u64 {
                    self.bits = [0; WORDS];
                } else {
                    for w in from + 1..=to {
                        self.bits[(w % WORDS as u64) as usize] = 0;
                    }
                }
                self.top = Some(n);
            }
            None => self.top = Some(n),
        }
        let (word, bit) = Self::slot(n);
        self.bits[word] |= bit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(w: &mut ReplayWindow, n: u64) -> bool {
        let fresh = w.fresh(n);
        if fresh {
            w.mark(n);
        }
        fresh
    }

    #[test]
    fn each_number_opens_once_in_any_order() {
        let mut w = ReplayWindow::new();
        for n in [0, 2, 1, 7, 5, 3, 4, 6] {
            assert!(opened(&mut w, n), "{n}");
        }
        for n in 0..8 {
            assert!(!w.fresh(n), "{n} again");
        }
        assert!(w.fresh(8));
    }

    #[test]
    fn the_window_edge_is_exact() {
        let mut w = ReplayWindow::new();
        let top = 5000;
        assert!(opened(&mut w, top));
        // The oldest number the window still holds, and the first it does not.
        assert!(w.fresh(top - (WINDOW - 1)));
        assert!(!w.fresh(top - WINDOW));
        assert!(!w.fresh(0));
        assert!(opened(&mut w, top - (WINDOW - 1)));
        assert!(!w.fresh(top - (WINDOW - 1)), "and only once");
        // Every number inside opens once.
        for n in top - (WINDOW - 2)..top {
            assert!(opened(&mut w, n), "{n}");
            assert!(!w.fresh(n));
        }
        // Moving up by one drops the oldest.
        assert!(opened(&mut w, top + 1));
        assert!(!w.fresh(top - (WINDOW - 1)));
        assert!(!w.fresh(top + 1 - WINDOW));
    }

    #[test]
    fn a_jump_past_the_window_forgets_everything_below_it() {
        let mut w = ReplayWindow::new();
        for n in 0..100 {
            assert!(opened(&mut w, n));
        }
        assert!(opened(&mut w, 100 + 10 * WINDOW));
        for n in 0..100 {
            assert!(!w.fresh(n), "{n} is too old now");
        }
        // Numbers just below the new top are fresh: their words were cleared,
        // not left holding the old numbers' bits.
        for n in 100 + 9 * WINDOW + 1..100 + 10 * WINDOW {
            assert!(w.fresh(n), "{n}");
        }
    }

    #[test]
    fn moving_by_less_than_the_ring_keeps_what_is_still_inside() {
        let mut w = ReplayWindow::new();
        let mut seen = std::collections::HashSet::new();
        for n in (0..3000).step_by(3) {
            assert!(opened(&mut w, n));
            seen.insert(n);
        }
        let mut top = 2997;
        // Each step up, by less than a word, by a word or so and by most of
        // the window, keeps every number still inside exact.
        for step in [0, 1, 63, 64, 65, 500, 1023, 1024, 1087, 1088] {
            if step > 0 {
                top += step;
                assert!(opened(&mut w, top));
                seen.insert(top);
            }
            for n in top.saturating_sub(WINDOW + 70)..=top + 70 {
                let fresh = n > top || (top - n < WINDOW && !seen.contains(&n));
                assert_eq!(w.fresh(n), fresh, "{n} with {top} the highest");
            }
        }
    }

    #[test]
    fn numbers_near_the_top_of_the_range_work() {
        let mut w = ReplayWindow::new();
        let top = u64::MAX - 1;
        assert!(opened(&mut w, top - 5));
        assert!(opened(&mut w, top));
        assert!(!w.fresh(top - 5));
        assert!(w.fresh(top - 4));
        assert!(!w.fresh(top - WINDOW));
    }
}
