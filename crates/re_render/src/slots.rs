//! Generation-checked slots: the storage behind mesh, texture and material handles.
//!
//! A slot's generation goes up every time its value is removed, so a handle to a
//! released value never reaches the value that later reuses the slot.

/// A slot vector with a free list.
#[derive(Debug)]
pub(crate) struct Slots<T> {
    entries: Vec<Entry<T>>,
    free: Vec<u32>,
    live: usize,
}

#[derive(Debug)]
struct Entry<T> {
    generation: u32,
    value: Option<T>,
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Slots {
            entries: Vec::new(),
            free: Vec::new(),
            live: 0,
        }
    }
}

impl<T> Slots<T> {
    /// Stores `value` and returns its `(index, generation)`.
    pub fn insert(&mut self, value: T) -> (u32, u32) {
        self.live += 1;
        if let Some(index) = self.free.pop() {
            let entry = &mut self.entries[index as usize];
            entry.value = Some(value);
            (index, entry.generation)
        } else {
            let index = u32::try_from(self.entries.len()).expect("more than 2^32 GPU resources");
            self.entries.push(Entry {
                generation: 0,
                value: Some(value),
            });
            (index, 0)
        }
    }

    /// The value at `index` if it is live and still at `generation`.
    pub fn get(&self, index: u32, generation: u32) -> Option<&T> {
        self.entries
            .get(index as usize)
            .filter(|e| e.generation == generation)
            .and_then(|e| e.value.as_ref())
    }

    /// Removes and returns the value at `index` if it is live and at `generation`.
    pub fn remove(&mut self, index: u32, generation: u32) -> Option<T> {
        let entry = self
            .entries
            .get_mut(index as usize)
            .filter(|e| e.generation == generation)?;
        let value = entry.value.take()?;
        // A slot whose generation would wrap is retired instead of reused, so an
        // old handle can never match again.
        match entry.generation.checked_add(1) {
            Some(next) => {
                entry.generation = next;
                self.free.push(index);
            }
            None => entry.generation = u32::MAX,
        }
        self.live -= 1;
        Some(value)
    }

    /// The number of live values.
    pub fn len(&self) -> usize {
        self.live
    }

    /// The live values.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().filter_map(|e| e.value.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_handles_do_not_match_reused_slots() {
        let mut slots = Slots::default();
        let (a, ga) = slots.insert("a");
        let (b, gb) = slots.insert("b");
        assert_eq!(slots.len(), 2);
        assert_eq!(slots.remove(a, ga), Some("a"));
        assert_eq!(slots.remove(a, ga), None, "double release");
        let (c, gc) = slots.insert("c");
        assert_eq!(c, a, "the slot is reused");
        assert_ne!(gc, ga, "with a new generation");
        assert_eq!(slots.get(a, ga), None);
        assert_eq!(slots.get(c, gc), Some(&"c"));
        assert_eq!(slots.get(b, gb), Some(&"b"));
        assert_eq!(slots.get(99, 0), None);
        assert_eq!(slots.len(), 2);
        assert_eq!(slots.values().count(), 2);
    }
}
