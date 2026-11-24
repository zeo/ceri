// the engine's hash maps and sets: keyed mostly by node ids, addresses and short names, on the
// app's own content, so they hash with a multiply and a rotate where the standard library's
// default spends a keyed SipHash on every lookup

use std::hash::{BuildHasherDefault, Hasher};

const SEED: u64 = 0xf135_7aea_2e62_a9c5;

#[derive(Clone, Copy, Default)]
pub struct Fast(u64);

impl Fast {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = self.0.wrapping_add(word).wrapping_mul(SEED);
    }
}

impl Hasher for Fast {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_le_bytes(*word));
        }
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            // the length keeps a short tail apart from the same bytes followed by zeros
            self.add(u64::from_le_bytes(last) ^ (rest.len() as u64) << 56);
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    // the multiply leaves its best bits at the top, and the tables index by the low ones
    #[inline]
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }
}

pub type FastState = BuildHasherDefault<Fast>;
pub type HashMap<K, V> = std::collections::HashMap<K, V, FastState>;
pub type HashSet<K> = std::collections::HashSet<K, FastState>;

#[cfg(test)]
mod tests {
    use super::{Fast, HashMap};
    use std::hash::Hasher;

    #[test]
    fn nearby_keys_and_short_strings_spread_over_the_low_bits_a_table_indexes_by() {
        let low = |hash: u64| hash & 0xff;
        let mut seen = std::collections::BTreeSet::new();
        for key in 0..256u64 {
            let mut hasher = Fast::default();
            hasher.write_u64(key);
            seen.insert(low(hasher.finish()));
        }
        assert!(
            seen.len() > 150,
            "only {} of 256 low bytes used",
            seen.len()
        );
        let hash = |text: &str| {
            let mut hasher = Fast::default();
            hasher.write(text.as_bytes());
            hasher.finish()
        };
        assert_ne!(hash("ab"), hash("ab\0"));
        let mut map: HashMap<&str, u32> = HashMap::default();
        map.insert("width", 1);
        map.insert("height", 2);
        assert_eq!(map["width"] + map["height"], 3);
    }
}
