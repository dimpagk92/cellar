//! Locality-sensitive hashing for tree-snapshot dedup.
//!
//! Pattern adapted from screenpipe-a11y (MIT). Word-level 3-shingles fed into
//! a 64-bit SimHash accumulator; similar text → small Hamming distance.
//! Scrolling a page typically changes 5–10 bits out of 64, so a threshold
//! of ~10 reliably skips near-duplicate content.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// 64-bit content hash (exact equality).
pub fn content_hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// 64-bit SimHash over word-level 3-shingles.
/// Empty / whitespace-only input returns 0.
pub fn simhash(text: &str) -> u64 {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return 0;
    }

    let mut bits = [0i32; 64];
    let shingle_size = 3.min(words.len());

    for window in words.windows(shingle_size) {
        let mut h = DefaultHasher::new();
        for w in window {
            w.hash(&mut h);
        }
        let hash = h.finish();
        for (i, bit) in bits.iter_mut().enumerate() {
            if (hash >> i) & 1 == 1 {
                *bit += 1;
            } else {
                *bit -= 1;
            }
        }
    }

    let mut result: u64 = 0;
    for (i, &bit) in bits.iter().enumerate() {
        if bit > 0 {
            result |= 1 << i;
        }
    }
    result
}

/// Number of differing bits between two 64-bit hashes.
pub fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic() {
        assert_eq!(content_hash("hello world"), content_hash("hello world"));
        assert_ne!(content_hash("a"), content_hash("b"));
    }

    #[test]
    fn simhash_identical_text_matches() {
        let t = "the quick brown fox jumps over the lazy dog";
        assert_eq!(hamming_distance(simhash(t), simhash(t)), 0);
    }

    #[test]
    fn simhash_scrolled_page_is_close() {
        // A realistic doc page where scrolling exposes 2 new lines.
        let base = "Welcome to the documentation site\n\
            Getting started with the framework\n\
            Installation guide for new users\n\
            Configure your development environment\n\
            Set up the database connection\n\
            Create your first application\n\
            Understanding the project structure\n\
            Working with models and controllers\n\
            Routing and middleware configuration\n\
            Authentication and authorization setup\n\
            Testing your application thoroughly\n\
            Deployment best practices guide\n\
            Performance optimization techniques\n\
            Monitoring and logging setup\n\
            Troubleshooting common issues here\n\
            Community support and resources\n\
            Contributing to the project\n\
            License and copyright information";
        let scrolled = "Welcome to the documentation site\n\
            Getting started with the framework\n\
            Installation guide for new users\n\
            Configure your development environment\n\
            Set up the database connection\n\
            Create your first application\n\
            Understanding the project structure\n\
            Working with models and controllers\n\
            Routing and middleware configuration\n\
            Authentication and authorization setup\n\
            Testing your application thoroughly\n\
            Deployment best practices guide\n\
            Performance optimization techniques\n\
            Monitoring and logging setup\n\
            Troubleshooting common issues here\n\
            Community support and resources\n\
            Frequently asked questions page\n\
            API reference documentation here";
        let d = hamming_distance(simhash(base), simhash(scrolled));
        assert!(d <= 10, "scroll hamming distance should be small, got {d}");
    }

    #[test]
    fn simhash_different_topics_are_far_apart() {
        let a = simhash(
            "the quick brown fox jumps over the lazy dog and runs through the forest \
             chasing rabbits while the sun sets behind the mountains",
        );
        let b = simhash(
            "rust programming language provides memory safety without garbage collection \
             enabling developers to build reliable and efficient software systems",
        );
        assert!(hamming_distance(a, b) > 10);
    }

    #[test]
    fn simhash_empty_is_zero() {
        assert_eq!(simhash(""), 0);
        assert_eq!(simhash("   "), 0);
    }

    #[test]
    fn hamming_distance_basic() {
        assert_eq!(hamming_distance(0, 0), 0);
        assert_eq!(hamming_distance(0b1111, 0b0000), 4);
        assert_eq!(hamming_distance(u64::MAX, 0), 64);
    }
}
