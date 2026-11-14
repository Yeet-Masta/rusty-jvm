//! A fast, non-cryptographic hasher for the VM's own lookup tables.
//!
//! The tables keyed by class names are consulted from the interpreter's hot paths, where the
//! standard library's SipHash costs more than the lookup it serves. Their keys come from class
//! files the VM already trusts with far more than a slow table, so flood resistance buys nothing.

use std::hash::{BuildHasherDefault, Hasher};

/// Folds the input eight bytes at a time with a rotate, xor and multiply, then mixes the result
/// so that both its high bits (which pick a `DashMap` shard) and its low bits (which pick a
/// bucket) depend on every input byte.
#[derive(Clone, Copy, Default)]
pub(crate) struct FastHasher(u64);

pub(crate) type FastBuildHasher = BuildHasherDefault<FastHasher>;

impl FastHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517C_C1B7_2722_0A95);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word) ^ ((rest.len() as u64) << 56));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        let mut hash = self.0;
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        hash ^ (hash >> 33)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::hash::BuildHasher;

    #[test]
    fn distinguishes_similar_class_names() {
        let build = FastBuildHasher::default();
        let names = [
            "java/lang/Object",
            "java/lang/Objects",
            "java/lang/String",
            "java/lang/StringBuilder",
            "[Ljava/lang/String;",
            "[[Ljava/lang/String;",
            "",
            "a",
            "b",
        ];
        let hashes: HashSet<u64> = names.iter().map(|name| build.hash_one(name)).collect();
        assert_eq!(hashes.len(), names.len());
    }

    #[test]
    fn spreads_short_keys_over_high_and_low_bits() {
        let build = FastBuildHasher::default();
        let high: HashSet<u64> = (0..64u32)
            .map(|i| build.hash_one(format!("C{i}")) >> 58)
            .collect();
        let low: HashSet<u64> = (0..64u32)
            .map(|i| build.hash_one(format!("C{i}")) & 63)
            .collect();
        assert!(high.len() > 24 && low.len() > 24);
    }
}
