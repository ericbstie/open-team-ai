//! The pinned per-completion RNG derivation (ADR 0025).
//!
//! Every seeded choice in a completion draws from a single per-completion RNG
//! keyed on ADR 0008's determinism tuple `(seed, user, call_seq)`:
//!
//! ```text
//! seed_bytes = fnv1a64( LEN(seed_u64_le) ‖ seed_u64_le
//!                     ‖ LEN(user_utf8)   ‖ user_utf8
//!                     ‖ LEN(call_seq_le) ‖ call_seq_u64_le )
//! rng        = ChaCha8Rng::seed_from_u64(seed_bytes)
//! ```
//!
//! The length-delimited canonical encoding is load-bearing (ADR 0025): each
//! field is prefixed with its byte length as u64 LE, preventing the
//! field-boundary collisions a bare concatenation would admit. FNV-1a-64 is
//! hand-rolled (zero new dep — the same hash family as the ADR 0014 embedder);
//! the 64-bit funnel is accepted, and `seed_from_u64`'s SplitMix expansion
//! fills ChaCha8's 32-byte seed deterministically within the pinned
//! rand_chacha 0.10. This derivation lives here, in `openteam-mock` (the
//! harness never derives the RNG), so the contract tests provably agree with
//! the mock by calling the identical function.

use rand::SeedableRng as _;
use rand_chacha::ChaCha8Rng;

use openteam_wire::Seed;

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Hand-rolled FNV-1a-64 (ADR 0025; same family as the ADR 0014 embedder hash).
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// The length-delimited canonical encoding: each field prefixed by its byte
/// length as u64 LE (ADR 0025 — prevents field-boundary collisions).
fn encode_fields(fields: &[&[u8]]) -> Vec<u8> {
    let total: usize = fields.iter().map(|field| 8 + field.len()).sum();
    let mut buf = Vec::with_capacity(total);
    for field in fields {
        buf.extend_from_slice(&(field.len() as u64).to_le_bytes());
        buf.extend_from_slice(field);
    }
    buf
}

/// Derive the per-completion RNG from the determinism tuple (ADR 0008/0025).
pub fn derive_rng(seed: Seed, user: &str, call_seq: u64) -> ChaCha8Rng {
    let encoded = encode_fields(&[
        &seed.to_le_bytes(),
        user.as_bytes(),
        &call_seq.to_le_bytes(),
    ]);
    ChaCha8Rng::seed_from_u64(fnv1a64(&encoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngExt as _;

    fn first_draws(seed: Seed, user: &str, call_seq: u64) -> [u64; 4] {
        let mut rng = derive_rng(seed, user, call_seq);
        [rng.random(), rng.random(), rng.random(), rng.random()]
    }

    #[test]
    fn fnv1a64_matches_reference_vectors() {
        // The published FNV-1a-64 vectors: empty input is the offset basis.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn same_tuple_yields_the_same_stream() {
        assert_eq!(
            first_draws(42, "orchestrator", 0),
            first_draws(42, "orchestrator", 0)
        );
        assert_eq!(
            first_draws(u64::MAX, "team-agent:agent-3:generalist", 7),
            first_draws(u64::MAX, "team-agent:agent-3:generalist", 7)
        );
    }

    #[test]
    fn each_field_decorrelates_the_stream() {
        let base = first_draws(42, "orchestrator", 0);
        assert_ne!(base, first_draws(43, "orchestrator", 0), "seed must matter");
        assert_ne!(
            base,
            first_draws(42, "meta-agent:meta-1", 0),
            "user must matter"
        );
        assert_ne!(base, first_draws(42, "orchestrator", 1), "call_seq matters");
    }

    #[test]
    fn length_delimiting_prevents_field_boundary_collisions() {
        // A bare concatenation would encode ("ab","c") and ("a","bc")
        // identically; the length prefixes must split them.
        let left = encode_fields(&[b"ab", b"c"]);
        let right = encode_fields(&[b"a", b"bc"]);
        assert_ne!(left, right);
        assert_ne!(fnv1a64(&left), fnv1a64(&right));
    }

    #[test]
    fn edge_seeds_produce_distinct_streams() {
        let edges = [0_u64, 1, u64::MAX, 4_294_967_291, 2_305_843_009_213_693_951];
        let draws: Vec<[u64; 4]> = edges
            .iter()
            .map(|&seed| first_draws(seed, "orchestrator", 0))
            .collect();
        for i in 0..draws.len() {
            for j in (i + 1)..draws.len() {
                assert_ne!(draws[i], draws[j], "seeds {} vs {}", edges[i], edges[j]);
            }
        }
    }
}
