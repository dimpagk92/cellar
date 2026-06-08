//! [`Tokenizer`] trait + default impls.
//!
//! The builder uses a [`Tokenizer`] to re-measure every
//! [`crate::source::Contribution`] before pruning, so a source that
//! over-estimates `estimated_tokens` can't blow the budget.
//!
//! Two impls ship in the crate:
//! - [`CharApproxTokenizer`] — default; 1 token ≈ 4 chars rule of thumb.
//!   Zero runtime dependencies, fine for development and tests, but not
//!   accurate enough for production budget enforcement against a real
//!   provider.
//! - `TiktokenCl100k` — behind the `tiktoken` feature. Uses
//!   the `tiktoken-rs` crate for ground-truth OpenAI `cl100k_base` counts. Suitable
//!   for any provider whose tokenizer is close to GPT-4 / GPT-3.5
//!   (Anthropic's Claude is close enough for budget purposes; for exact
//!   Claude counts, implement [`Tokenizer`] with a Claude-specific tokenizer
//!   and pass it to the builder).

use std::sync::Arc;

/// Counts tokens for a `&str`.
///
/// Implementations must be cheap and `Send + Sync` because the builder calls
/// `count` once per contribution per turn.
pub trait Tokenizer: Send + Sync {
    /// Number of tokens the model would see for `text`.
    fn count(&self, text: &str) -> usize;

    /// Token IDs for providers that need them. Default returns `None`; only
    /// tokenizers that have a real BPE backing (e.g. tiktoken) override.
    fn encode(&self, _text: &str) -> Option<Vec<u32>> {
        None
    }
}

/// Trivial tokenizer using the well-known "≈ 4 chars per token" rule of
/// thumb. Default for the builder so the crate has no runtime model assets.
///
/// Counts bytes (not unicode scalar values) divided by 4, rounded up, with
/// an extra `+1` floor so any non-empty input reports at least 1 token. This
/// matches what most sources will report in `estimated_tokens`, so the
/// builder won't surprise sources during pruning at default settings.
///
/// **Not accurate for billing or budgeting against a real LLM provider** —
/// enable the `tiktoken` feature and use `TiktokenCl100k` when accuracy
/// matters.
#[derive(Debug, Clone, Copy, Default)]
pub struct CharApproxTokenizer;

impl CharApproxTokenizer {
    /// Construct a new [`CharApproxTokenizer`].
    pub fn new() -> Self {
        CharApproxTokenizer
    }
}

impl Tokenizer for CharApproxTokenizer {
    fn count(&self, text: &str) -> usize {
        if text.is_empty() {
            0
        } else {
            text.len().div_ceil(4)
        }
    }
}

/// Convenience: every `Arc<dyn Tokenizer>` already satisfies [`Tokenizer`]
/// via blanket deref-style forwarding. This impl lets the builder hold an
/// `Arc<dyn Tokenizer>` and pass it around without unwrapping.
impl<T: Tokenizer + ?Sized> Tokenizer for Arc<T> {
    fn count(&self, text: &str) -> usize {
        (**self).count(text)
    }

    fn encode(&self, text: &str) -> Option<Vec<u32>> {
        (**self).encode(text)
    }
}

#[cfg(feature = "tiktoken")]
mod tiktoken_impl {
    use super::Tokenizer;

    use tiktoken_rs::{cl100k_base, CoreBPE};

    /// Production-grade tokenizer using OpenAI's `cl100k_base` encoding via
    /// the `tiktoken-rs` crate. Available behind the `tiktoken` feature.
    ///
    /// Construction is cheap (BPE tables are loaded lazily by `cl100k_base`),
    /// but the first `count` call after process start pays the load cost.
    /// Reuse one instance across turns.
    pub struct TiktokenCl100k {
        bpe: CoreBPE,
    }

    impl TiktokenCl100k {
        /// Construct a new [`TiktokenCl100k`]. Returns `Err` only if
        /// `tiktoken-rs` fails to load its embedded BPE assets, which in
        /// practice never happens for `cl100k_base`.
        pub fn new() -> Result<Self, String> {
            cl100k_base()
                .map(|bpe| TiktokenCl100k { bpe })
                .map_err(|e| e.to_string())
        }
    }

    impl std::fmt::Debug for TiktokenCl100k {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("TiktokenCl100k").finish()
        }
    }

    impl Tokenizer for TiktokenCl100k {
        fn count(&self, text: &str) -> usize {
            // `encode_with_special_tokens` matches what OpenAI bills.
            self.bpe.encode_with_special_tokens(text).len()
        }

        fn encode(&self, text: &str) -> Option<Vec<u32>> {
            Some(self.bpe.encode_with_special_tokens(text))
        }
    }
}

#[cfg(feature = "tiktoken")]
pub use tiktoken_impl::TiktokenCl100k;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_approx_returns_zero_for_empty_string() {
        let tok = CharApproxTokenizer::new();
        assert_eq!(tok.count(""), 0);
    }

    #[test]
    fn char_approx_uses_four_chars_per_token() {
        let tok = CharApproxTokenizer;
        assert_eq!(tok.count("a"), 1);
        assert_eq!(tok.count("abcd"), 1);
        assert_eq!(tok.count("abcde"), 2);
        assert_eq!(tok.count("abcdefgh"), 2);
        assert_eq!(tok.count("abcdefghi"), 3);
    }

    #[test]
    fn char_approx_encode_returns_none() {
        let tok = CharApproxTokenizer;
        assert!(tok.encode("anything").is_none());
    }

    #[test]
    fn arc_dyn_tokenizer_forwards() {
        let tok: Arc<dyn Tokenizer> = Arc::new(CharApproxTokenizer);
        assert_eq!(tok.count("abcd"), 1);
        assert!(tok.encode("abcd").is_none());
    }

    #[cfg(feature = "tiktoken")]
    #[test]
    fn tiktoken_cl100k_counts_a_short_string() {
        let tok = TiktokenCl100k::new().expect("load cl100k");
        // "hello world" is 2 tokens in cl100k_base.
        let count = tok.count("hello world");
        assert!(
            count > 0 && count <= 5,
            "got {count} tokens for 'hello world'"
        );

        let ids = tok.encode("hello world").expect("ids");
        assert_eq!(ids.len(), count, "encode/count agreement");
    }
}
