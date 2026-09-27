use std::collections::HashMap;
use std::hash::Hash;

/// A cache holding at most `capacity` entries that evicts the least recently used one.
pub struct LruCache<K, V> {
    capacity: usize,
    entries: HashMap<K, (V, u64)>,
    clock: u64,
}

impl<K: Eq + Hash + Clone, V> LruCache<K, V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            clock: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let now = self.tick();
        let entry = self.entries.get_mut(key)?;
        entry.1 = now;
        Some(&entry.0)
    }

    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        if self.capacity == 0 {
            return None;
        }
        let now = self.tick();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.1 = now;
            return Some(std::mem::replace(&mut entry.0, value));
        }
        if self.entries.len() == self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, (value, now));
        None
    }

    pub fn peek(&self, key: &K) -> Option<&V> {
        self.entries.get(key).map(|(value, _)| value)
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.entries.remove(key).map(|(value, _)| value)
    }

    pub fn keys(&self) -> Vec<K> {
        let mut keyed: Vec<(&K, u64)> = self.entries.iter().map(|(k, (_, t))| (k, *t)).collect();
        keyed.sort_by(|a, b| b.1.cmp(&a.1));
        keyed.into_iter().map(|(k, _)| k.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_empty() {
        let cache: LruCache<u8, u8> = LruCache::new(2);
        assert!(cache.is_empty());
        assert_eq!(cache.capacity(), 2);
    }

    #[test]
    fn eviction_order() {
        let mut cache = LruCache::new(2);
        cache.put(1, 'a');
        cache.put(2, 'b');
        cache.get(&1);
        cache.put(3, 'c');
        assert_eq!(cache.keys(), [3, 1]);
    }
}
