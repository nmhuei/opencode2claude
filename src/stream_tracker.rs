//! SseBlockTracker — Encapsulates the block-index state machine for streaming SSE.
//!
//! Manages the lifecycle of content blocks (thinking, text, tool_use) during
//! Anthropic SSE streaming. Tracks which blocks are open, assigns sequential
//! indices, and provides methods for controlled state transitions.
//!
//! # State Machine
//!
//! At most one thinking block and one text block can be open at a time.
//! Multiple tool use blocks can be open concurrently (keyed by call index).
//! Opening a thinking block auto-closes any open text block, and vice versa.
//! Opening a tool use block auto-closes both thinking and text blocks.

use std::collections::HashMap;

/// Tracks the state of content blocks during SSE streaming.
///
/// Encapsulates the 7+ loose state variables from `forward_to_llm_stream`
/// into a single state machine that enforces correct block lifecycle.
#[derive(Debug)]
pub struct SseBlockTracker {
    /// Index of the currently open thinking block, if any.
    thinking_idx: Option<usize>,
    /// Index of the currently open text block, if any.
    text_idx: Option<usize>,
    /// Open tool use blocks: call_tc_index → (block_index, id, name).
    tool_indices: HashMap<usize, (usize, String, String)>,
    /// Next content block index to assign.
    next_idx: usize,
    /// Whether any block has ever been opened (tracked for fallback detection).
    ever_opened: bool,
}

impl SseBlockTracker {
    /// Create a new tracker with no open blocks.
    pub fn new() -> Self {
        Self {
            thinking_idx: None,
            text_idx: None,
            tool_indices: HashMap::new(),
            next_idx: 0,
            ever_opened: false,
        }
    }

    /// Return true if any block has ever been opened since creation.
    /// Used to detect "empty stream" scenarios where message_start was sent
    /// but upstream returned zero content deltas.
    pub fn has_any_blocks_ever_opened(&self) -> bool {
        self.ever_opened
    }

    /// Number of content block indices allocated since creation. Blocks are
    /// never deallocated, so the difference between two snapshots is the
    /// number of blocks opened in between — the per-attempt emission signal
    /// for retry gates that must not reset the tracker.
    pub fn allocated_blocks(&self) -> usize {
        self.next_idx
    }

    /// Allocate and return the next content block index.
    pub fn next_index(&mut self) -> usize {
        let i = self.next_idx;
        self.next_idx += 1;
        self.ever_opened = true;
        i
    }

    /// Returns the current thinking block index, if open.
    pub fn thinking_idx(&self) -> Option<usize> {
        self.thinking_idx
    }

    /// Returns the current text block index, if open.
    pub fn text_idx(&self) -> Option<usize> {
        self.text_idx
    }

    /// Returns true if any thinking or text block is open.
    pub fn has_open_blocks(&self) -> bool {
        self.thinking_idx.is_some() || self.text_idx.is_some() || !self.tool_indices.is_empty()
    }

    /// Get the tool block info for a given call index, if open.
    /// Returns `(block_index, id, name)`.
    pub fn tool_idx(&self, call_idx: usize) -> Option<&(usize, String, String)> {
        self.tool_indices.get(&call_idx)
    }

    // ── Ensure Operations (get-or-create) ──

    /// Ensure a thinking block is open, creating one if needed.
    ///
    /// If thinking is already open, returns the existing index with `is_new: false`.
    /// If a text block was open, it is automatically closed and its index returned.
    /// Returns `(thinking_index, is_new, optional_closed_text_index)`.
    pub fn ensure_thinking(&mut self) -> (usize, bool, Option<usize>) {
        if let Some(idx) = self.thinking_idx {
            return (idx, false, None);
        }
        let closed_text = self.text_idx.take();
        let idx = self.next_index();
        self.thinking_idx = Some(idx);
        (idx, true, closed_text)
    }

    /// Ensure a text block is open, creating one if needed.
    ///
    /// If text is already open, returns the existing index with `is_new: false`.
    /// If a thinking block was open, it is automatically closed and its index returned.
    /// Returns `(text_index, is_new, optional_closed_thinking_index)`.
    pub fn ensure_text(&mut self) -> (usize, bool, Option<usize>) {
        if let Some(idx) = self.text_idx {
            return (idx, false, None);
        }
        let closed_thinking = self.thinking_idx.take();
        let idx = self.next_index();
        self.text_idx = Some(idx);
        (idx, true, closed_thinking)
    }

    // ── Block Close Operations ──

    /// Close the thinking block, returning its index (None if not open).
    pub fn close_thinking(&mut self) -> Option<usize> {
        self.thinking_idx.take()
    }

    /// Close the text block, returning its index (None if not open).
    pub fn close_text(&mut self) -> Option<usize> {
        self.text_idx.take()
    }

    /// Close a specific tool use block by its call index.
    /// Returns the block index, id, and name if it was open.
    pub fn close_tool_use(&mut self, call_idx: usize) -> Option<(usize, String, String)> {
        self.tool_indices.remove(&call_idx)
    }

    /// Close all open blocks, returning a list of `("type", block_index)` pairs.
    ///
    /// Order: thinking, text, then all tool blocks.
    pub fn close_all(&mut self) -> Vec<(&'static str, usize)> {
        let mut closed = Vec::new();

        if let Some(idx) = self.thinking_idx.take() {
            closed.push(("thinking", idx));
        }
        if let Some(idx) = self.text_idx.take() {
            closed.push(("text", idx));
        }
        for (_, (idx, _, _)) in self.tool_indices.drain() {
            closed.push(("tool_use", idx));
        }

        closed
    }

    // ── Block Open Operations ──

    /// Open a thinking block.
    ///
    /// If a text block was open, it is automatically closed.
    /// Returns `(thinking_index, optional_closed_text_index)`.
    pub fn open_thinking(&mut self) -> (usize, Option<usize>) {
        let closed_text = self.text_idx.take();
        let idx = self.next_index();
        self.thinking_idx = Some(idx);
        (idx, closed_text)
    }

    /// Open a text block.
    ///
    /// If a thinking block was open, it is automatically closed.
    /// Returns `(text_index, optional_closed_thinking_index)`.
    pub fn open_text(&mut self) -> (usize, Option<usize>) {
        let closed_thinking = self.thinking_idx.take();
        let idx = self.next_index();
        self.text_idx = Some(idx);
        (idx, closed_thinking)
    }

    /// Open a tool use block, auto-closing thinking and text blocks first.
    ///
    /// Returns `(block_index, optional_closed_thinking, optional_closed_text)`.
    pub fn open_tool_use(
        &mut self,
        call_idx: usize,
        id: String,
        name: String,
    ) -> (usize, Option<usize>, Option<usize>) {
        let closed_thinking = self.thinking_idx.take();
        let closed_text = self.text_idx.take();
        let block_idx = self.next_index();
        self.tool_indices.insert(call_idx, (block_idx, id, name));
        (block_idx, closed_thinking, closed_text)
    }

    // ── Full Reset ──

    /// Fully reset the tracker to its initial state.
    ///
    /// Unlike `close_all()` which only closes active blocks, this also resets
    /// `ever_opened` and `next_idx` back to zero. Used between search intercept
    /// loop iterations so the next stream starts with a clean slate.
    pub fn reset(&mut self) {
        self.thinking_idx = None;
        self.text_idx = None;
        self.tool_indices.clear();
        self.next_idx = 0;
        self.ever_opened = false;
    }
}

impl Default for SseBlockTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_tracker_has_no_blocks() {
        let mut tracker = SseBlockTracker::new();
        assert!(tracker.thinking_idx().is_none());
        assert!(tracker.text_idx().is_none());
        assert!(!tracker.has_open_blocks());
        assert_eq!(tracker.close_all().len(), 0);
    }

    #[test]
    fn test_open_thinking_assigns_index() {
        let mut t = SseBlockTracker::new();
        let (idx, closed_text) = t.open_thinking();
        assert_eq!(idx, 0);
        assert!(closed_text.is_none());
        assert_eq!(t.thinking_idx(), Some(0));
    }

    #[test]
    fn test_open_thinking_closes_text() {
        let mut t = SseBlockTracker::new();
        t.open_text(); // idx 0
        let (idx, closed_text) = t.open_thinking(); // idx 1
        assert_eq!(idx, 1);
        assert_eq!(closed_text, Some(0));
        assert!(t.text_idx().is_none());
    }

    #[test]
    fn test_open_text_closes_thinking() {
        let mut t = SseBlockTracker::new();
        t.open_thinking(); // idx 0
        let (idx, closed_thinking) = t.open_text(); // idx 1
        assert_eq!(idx, 1);
        assert_eq!(closed_thinking, Some(0));
        assert!(t.thinking_idx().is_none());
    }

    #[test]
    fn test_open_tool_use_auto_closes() {
        let mut t = SseBlockTracker::new();
        t.open_thinking(); // idx 0
                           // open_text auto-closes thinking
        let (text_idx, closed_thinking_from_text) = t.open_text(); // idx 1
        assert_eq!(text_idx, 1);
        assert_eq!(closed_thinking_from_text, Some(0));

        // open_tool_use auto-closes text (already no thinking open)
        let (idx, closed_t, closed_x) = t.open_tool_use(0, "toolu_abc".into(), "bash".into());
        assert_eq!(idx, 2);
        assert!(closed_t.is_none()); // thinking already closed by open_text
        assert_eq!(closed_x, Some(1)); // text closed by open_tool_use
    }

    #[test]
    fn test_close_thinking() {
        let mut t = SseBlockTracker::new();
        t.open_thinking();
        assert_eq!(t.close_thinking(), Some(0));
        assert!(t.thinking_idx().is_none());
    }

    #[test]
    fn test_close_text() {
        let mut t = SseBlockTracker::new();
        t.open_text();
        assert_eq!(t.close_text(), Some(0));
        assert!(t.text_idx().is_none());
    }

    #[test]
    fn test_close_tool_use() {
        let mut t = SseBlockTracker::new();
        t.open_tool_use(7, "toolu_abc".into(), "bash".into());
        let result = t.close_tool_use(7);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, 0); // block index
        assert!(t.close_tool_use(7).is_none()); // already closed
    }

    #[test]
    fn test_close_all_empties_everything() {
        let mut t = SseBlockTracker::new();
        t.open_thinking(); // idx 0 — thinking open
                           // open_tool_use closes thinking and text (none open), opens tool
        t.open_tool_use(0, "toolu_a".into(), "bash".into()); // idx 1
                                                             // open_text — only tool blocks remain (thinking/closed already), opens text
        t.open_text(); // idx 2 — text_idx=Some(2)
                       // open_tool_use closes text, opens another tool
        t.open_tool_use(1, "toolu_b".into(), "python".into()); // idx 3

        // close_all: 2 tool blocks + text (already closed by open_tool_use)
        let closed = t.close_all();
        // Only tool blocks remain open (thinking+text auto-closed by tool_use opens)
        assert_eq!(closed.len(), 2, "expected 2 tool blocks, got {:?}", closed);
        assert!(!t.has_open_blocks());
        assert!(t.thinking_idx().is_none());
        assert!(t.text_idx().is_none());
    }

    #[test]
    fn test_sequential_indices() {
        let mut t = SseBlockTracker::new();
        assert_eq!(t.next_index(), 0);
        assert_eq!(t.next_index(), 1);
        assert_eq!(t.next_index(), 2);
    }

    #[test]
    fn test_open_without_close_returns_none() {
        let mut t = SseBlockTracker::new();
        assert!(t.close_thinking().is_none());
        assert!(t.close_text().is_none());
    }

    #[test]
    fn test_ensure_thinking_reuses_existing() {
        let mut t = SseBlockTracker::new();
        let (idx1, _, _) = t.ensure_thinking();
        // Second call reuses same index
        let (idx2, is_new, closed) = t.ensure_thinking();
        assert_eq!(idx2, 0);
        assert!(!is_new);
        assert!(closed.is_none());
        // Verify only one index was assigned
        assert_eq!(idx1, idx2);
    }

    #[test]
    fn test_ensure_thinking_closes_text() {
        let mut t = SseBlockTracker::new();
        t.ensure_text(); // idx 0
                         // ensure_thinking closes text and opens new thinking at idx 1
        let (idx, is_new, closed_text) = t.ensure_thinking();
        assert_eq!(idx, 1);
        assert!(is_new);
        assert_eq!(closed_text, Some(0));
    }

    #[test]
    fn test_ensure_text_reuses_existing() {
        let mut t = SseBlockTracker::new();
        let (idx1, _, _) = t.ensure_text();
        let (idx2, is_new, closed) = t.ensure_text();
        assert_eq!(idx2, 0);
        assert!(!is_new);
        assert!(closed.is_none());
        assert_eq!(idx1, idx2);
    }

    #[test]
    fn test_ensure_text_closes_thinking() {
        let mut t = SseBlockTracker::new();
        t.ensure_thinking(); // idx 0
        let (idx, is_new, closed_thinking) = t.ensure_text(); // idx 1
        assert_eq!(idx, 1);
        assert!(is_new);
        assert_eq!(closed_thinking, Some(0));
    }

    #[test]
    fn test_tool_idx_returns_none_for_missing() {
        let t = SseBlockTracker::new();
        assert!(t.tool_idx(42).is_none());
    }

    #[test]
    fn test_tool_idx_returns_info() {
        let mut t = SseBlockTracker::new();
        t.open_tool_use(0, "toolu_abc".into(), "bash".into());
        let info = t.tool_idx(0);
        assert!(info.is_some());
        assert_eq!(info.unwrap().0, 0);
        assert_eq!(info.unwrap().1, "toolu_abc");
    }

    #[test]
    fn test_reuse_after_close() {
        let mut t = SseBlockTracker::new();
        t.open_thinking(); // idx 0
        t.close_thinking();
        t.open_thinking(); // idx 1
        assert_eq!(t.thinking_idx(), Some(1));
    }

    #[test]
    fn test_reset_clears_all_state() {
        let mut t = SseBlockTracker::new();
        t.open_thinking(); // idx 0
        t.open_text(); // idx 1, closes thinking
        t.open_tool_use(0, "toolu_abc".into(), "bash".into()); // idx 2
        assert!(t.has_any_blocks_ever_opened());

        t.reset();

        // Everything should be back to initial state
        assert!(!t.has_any_blocks_ever_opened());
        assert!(!t.has_open_blocks());
        assert!(t.thinking_idx().is_none());
        assert!(t.text_idx().is_none());
        assert!(t.tool_idx(0).is_none());
        // next_index should start from 0 again
        assert_eq!(t.next_index(), 0);
    }

    #[test]
    fn test_close_all_keeps_indices_monotonic_without_reset() {
        let mut t = SseBlockTracker::new();
        t.open_thinking(); // idx 0
        assert_eq!(t.close_all().len(), 1);
        t.open_thinking(); // next segment must NOT reuse idx 0
        assert_eq!(t.thinking_idx(), Some(1));
        assert_eq!(t.close_all().len(), 1);
        // emulate a third loop iteration without reset()
        let (idx, _, _) = t.ensure_thinking();
        assert_eq!(idx, 2);
    }

    #[test]
    fn test_reset_between_search_loops() {
        let mut t = SseBlockTracker::new();

        // Simulate loop 1: open thinking + text, then close_all + reset
        t.open_thinking(); // idx 0
        t.open_text(); // idx 1
        let closed = t.close_all();
        assert_eq!(closed.len(), 1); // only text (thinking auto-closed by open_text)
        t.reset();

        // Simulate loop 2: tracker should behave as fresh
        assert!(!t.has_any_blocks_ever_opened());
        let (idx, _, _) = t.ensure_text();
        assert_eq!(idx, 0); // starts from 0 again
        assert!(t.has_any_blocks_ever_opened());
    }
}
