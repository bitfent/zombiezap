//! Seeded RNG, bit-compatible with the ShotAnte TS generator
//! (legacy/packages/shared/src/mapgen.ts: hashSeed + mulberry32).
//!
//! Pure u32 wrapping arithmetic + one exact power-of-two division — identical
//! on native and wasm32 by construction, and identical to the TS output so the
//! mapgen port can be golden-tested against the original.

/// FNV-1a over the seed string. Matches TS `hashSeed` for ASCII seeds (TS
/// iterates UTF-16 code units; we iterate bytes — the same thing for ASCII,
/// which is all we ever generate).
pub fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

/// Mulberry32 — TS-exact port. Yields f64 in [0, 1).
#[derive(Clone, Debug)]
pub struct Mulberry32(u32);

impl Mulberry32 {
    pub fn new(state: u32) -> Self {
        Mulberry32(state)
    }

    /// Seed exactly like TS `seededRandom(seed)`.
    pub fn from_seed(seed: &str) -> Self {
        Mulberry32(fnv1a(seed))
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let a = self.0;
        let mut t = (a ^ (a >> 15)).wrapping_mul(a | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        ((t ^ (t >> 14)) as f64) / 4294967296.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_known_values() {
        // FNV-1a offset basis for the empty string.
        assert_eq!(fnv1a(""), 2166136261);
        // Stability canaries: if these move, golden compatibility is broken.
        assert_eq!(fnv1a("a"), 0xe40c292c);
        assert_eq!(fnv1a("zz-golden-1"), fnv1a("zz-golden-1"));
        assert_ne!(fnv1a("zz-a"), fnv1a("zz-b"));
    }

    #[test]
    fn mulberry_is_deterministic_and_in_range() {
        let mut a = Mulberry32::from_seed("zz-a");
        let mut b = Mulberry32::from_seed("zz-a");
        for _ in 0..1000 {
            let v = a.next();
            assert_eq!(v, b.next());
            assert!((0.0..1.0).contains(&v));
        }
    }
}
