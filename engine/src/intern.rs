// parsed values keyed by their source text, so a copyable style can carry a small id; the
// oldest entry gives way once the table is full, so values animated frame by frame never
// exhaust it, and an id whose slot was reused stops resolving instead of meaning another value

use std::collections::HashMap;
use std::rc::Rc;

// styles live for one layout pass, which never needs this many distinct values at once
const CAPACITY: usize = 4096;

pub(crate) struct Table<T: ?Sized> {
    // each slot keeps the id that filled it, so a reused slot rejects the older id
    slots: Vec<(u32, Rc<str>, Rc<T>)>,
    ids: HashMap<Rc<str>, u32>,
    next: u32,
}

impl<T: ?Sized> Table<T> {
    pub(crate) fn new() -> Self {
        Self {
            slots: Vec::new(),
            ids: HashMap::new(),
            next: 0,
        }
    }

    pub(crate) fn find(&self, source: &str) -> Option<u32> {
        self.ids.get(source).copied()
    }

    pub(crate) fn insert(&mut self, source: &str, value: Rc<T>) -> u32 {
        if let Some(id) = self.find(source) {
            return id;
        }
        let id = self.next;
        // the capacity divides 2^32, so slots stay in step when the counter wraps
        self.next = self.next.wrapping_add(1);
        let source: Rc<str> = source.into();
        let entry = (id, source.clone(), value);
        match self.slots.get_mut(id as usize % CAPACITY) {
            Some(slot) => {
                let (_, evicted, _) = std::mem::replace(slot, entry);
                self.ids.remove(&evicted);
            }
            None => self.slots.push(entry),
        }
        self.ids.insert(source, id);
        id
    }

    pub(crate) fn get(&self, id: u32) -> Option<Rc<T>> {
        self.slots
            .get(id as usize % CAPACITY)
            .filter(|(owner, ..)| *owner == id)
            .map(|(_, _, value)| value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_table_evicts_its_oldest_entry_and_forgets_its_id() {
        let mut table = Table::<u32>::new();
        let first = table.insert("0", Rc::new(0));
        assert_eq!(table.insert("0", Rc::new(9)), first);
        for value in 1..=CAPACITY as u32 {
            table.insert(&value.to_string(), Rc::new(value));
        }
        assert_eq!(table.get(first), None);
        assert_eq!(table.find("0"), None);
        let last = table
            .find(&CAPACITY.to_string())
            .expect("the newest entry stays");
        assert_eq!(table.get(last).as_deref(), Some(&(CAPACITY as u32)));
        assert_eq!(table.slots.len(), CAPACITY);
        assert_eq!(table.ids.len(), CAPACITY);
    }
}
