use serde::{Deserialize, Serialize};

/// Half-open byte-offset range `[start, end)` into a [`SourceFile`](super::SourceFile).
///
/// Spans are produced by the frontend (lifted from `brush_parser::SourceLocation`)
/// and consumed by the rewriter, which sorts edits by span and splices in one pass.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(
            start <= end,
            "Span: start ({start}) must precede end ({end})"
        );
        Self { start, end }
    }

    pub fn point(at: usize) -> Self {
        Self { start: at, end: at }
    }

    pub fn len(self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Smallest span enclosing both inputs.
    pub fn merge(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }

    /// Returns true if `inner` is entirely within `self`.
    pub fn contains(self, inner: Span) -> bool {
        self.start <= inner.start && inner.end <= self.end
    }

    /// Slice the source text for this span, panicking on bounds error.
    pub fn slice(self, source: &str) -> &str {
        &source[self.start..self.end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_and_emptiness() {
        assert_eq!(Span::new(0, 0).len(), 0);
        assert!(Span::new(0, 0).is_empty());
        assert_eq!(Span::new(3, 7).len(), 4);
        assert!(!Span::new(3, 7).is_empty());
    }

    #[test]
    fn merge_takes_outer_bounds() {
        let merged = Span::new(2, 5).merge(Span::new(4, 9));
        assert_eq!(merged, Span::new(2, 9));
    }

    #[test]
    fn merge_handles_disjoint() {
        let merged = Span::new(0, 3).merge(Span::new(7, 10));
        assert_eq!(merged, Span::new(0, 10));
    }

    #[test]
    fn contains_checks_full_enclosure() {
        let outer = Span::new(0, 10);
        assert!(outer.contains(Span::new(0, 10)));
        assert!(outer.contains(Span::new(2, 8)));
        assert!(!outer.contains(Span::new(0, 11)));
        assert!(!outer.contains(Span::new(11, 12)));
    }

    #[test]
    fn slice_extracts_source_substring() {
        let src = "hello world";
        assert_eq!(Span::new(0, 5).slice(src), "hello");
        assert_eq!(Span::new(6, 11).slice(src), "world");
    }

    #[test]
    fn point_is_zero_length() {
        let p = Span::point(42);
        assert_eq!(p.start, 42);
        assert_eq!(p.end, 42);
        assert!(p.is_empty());
    }
}
