//! Experimental retained container paths. Flat `SegPath` remains the interchange
//! representation; no flattened vector is cached in a retained path.
use super::{Seg, SegPath};
use rustc_hash::FxHashMap;
#[cfg(feature = "runtime-diagnostics")]
use std::cell::Cell;
use std::fmt;
use std::mem::size_of;
use std::rc::{Rc, Weak};

const INLINE_ITER_DEPTH: usize = 16;
const POOL_ENTRY_LIMIT: usize = 4096;

/// Thread-local, cumulative diagnostics for the representation experiment.
/// Live counts include all owners on this thread, including external API owners;
/// take before/after snapshots instead of resetting counters under live paths.
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(feature = "runtime-diagnostics", derive(serde::Serialize))]
pub struct PrefixPathDiagnostics {
    /// Calls to the engine pool, including empty and deep paths.
    pub retain_calls: u64,
    /// Total input segments passed to the engine pool.
    pub input_segments: u64,
    /// Bounded nonempty inputs for which a previous weak cursor was available.
    /// Includes attempts whose weak tail had already expired.
    pub cursor_attempts: u64,
    /// Cursor attempts that reused at least one canonical prefix segment.
    pub cursor_hits: u64,
    /// Prefix segments reused without a hash-table lookup.
    pub reused_prefix_segments: u64,
    /// Inputs beyond the inline cursor depth; these use the original full lookup.
    pub cursor_deep_fallbacks: u64,
    /// Retained path bodies constructed, including non-Rc public values.
    pub handles_created: u64,
    /// Retained path bodies whose destructor has not run.
    pub handles_live: u64,
    /// Prefix nodes allocated by pool and direct constructors.
    pub nodes_created: u64,
    /// Prefix nodes with a strong owner; excludes weak-only reserved storage.
    pub nodes_live: u64,
    /// Maximum strongly live node count observed on this thread.
    pub nodes_peak: u64,
    /// Pool lookups that upgraded an existing prefix node.
    pub pool_hits: u64,
    /// Pool lookups that allocated a prefix node.
    pub pool_misses: u64,
    /// Whole bounded-pool clears before new insertions.
    pub pool_clears: u64,
    /// Explicit to_vec exports; no export is cached by PrefixPath.
    pub flat_exports: u64,
    /// Total segment count requested by explicit flat exports.
    pub flat_export_segments: u64,
    /// Borrowed iterators whose ancestry exceeded inline storage.
    pub iterator_spills: u64,
}

#[cfg(feature = "runtime-diagnostics")]
thread_local! {
    static COUNTERS: Cell<PrefixPathDiagnostics> = Cell::new(PrefixPathDiagnostics::default());
}

// Remove the whole counter expression, including closure creation and its
// captured operands, from feature-empty primary builds.
macro_rules! count {
    ($action:expr) => {
        #[cfg(feature = "runtime-diagnostics")]
        {
            record_count($action);
        }
    };
}

#[cfg(feature = "runtime-diagnostics")]
fn record_count(f: impl FnOnce(&mut PrefixPathDiagnostics)) {
    // Paths may be destroyed during thread-local teardown; diagnostics must not
    // turn their otherwise valid destruction into a panic.
    let _ = COUNTERS.try_with(|cell| {
        let mut counters = cell.get();
        f(&mut counters);
        cell.set(counters);
    });
}

/// Read cumulative diagnostics on the calling thread. Feature-empty builds
/// return zeros and compile out the counter TLS and all hot counter calls.
pub fn prefix_path_diagnostics() -> PrefixPathDiagnostics {
    #[cfg(feature = "runtime-diagnostics")]
    {
        COUNTERS.with(Cell::get)
    }
    #[cfg(not(feature = "runtime-diagnostics"))]
    {
        PrefixPathDiagnostics::default()
    }
}

/// Concrete layouts, excluding allocator rounding. The Rc header estimate is
/// explicit so a requested-allocation probe can verify it for its toolchain.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "runtime-diagnostics", derive(serde::Serialize))]
pub struct PrefixPathLayouts {
    /// Inline PrefixPath body size, excluding its optional outer Rc allocation.
    pub path_body: usize,
    /// Inline immutable node body size, excluding Rc counts.
    pub node_body: usize,
    /// Key and weak value payload size; excludes hash control/capacity overhead.
    pub pool_entry: usize,
    /// Pool struct size, excluding its hash allocation.
    pub pool_body: usize,
    /// Iterator stack body size, excluding a deep ancestry spill.
    pub iterator_body: usize,
    /// Two word-sized Rc counts; verify requested allocation layout separately.
    pub assumed_rc_header: usize,
}

/// Report this compiled prototype's representation layouts.
pub fn prefix_path_layouts() -> PrefixPathLayouts {
    PrefixPathLayouts {
        path_body: size_of::<PrefixPath>(),
        node_body: size_of::<PathNode>(),
        pool_entry: size_of::<((usize, Seg), Weak<PathNode>)>(),
        pool_body: size_of::<PrefixPathPool>(),
        iterator_body: size_of::<PrefixPathIter<'_>>(),
        assumed_rc_header: 2 * size_of::<usize>(),
    }
}

struct PathNode {
    parent: Option<Rc<PathNode>>,
    segment: Seg,
}

impl PathNode {
    fn new(parent: Option<Rc<PathNode>>, segment: Seg) -> Rc<Self> {
        count!(|c| {
            c.nodes_created += 1;
            c.nodes_live += 1;
            c.nodes_peak = c.nodes_peak.max(c.nodes_live);
        });
        Rc::new(Self { parent, segment })
    }
}

impl Drop for PathNode {
    fn drop(&mut self) {
        count!(|c| c.nodes_live -= 1);
        // Release a uniquely owned chain iteratively. Path depth must not turn
        // a valid public API value into a recursive-drop stack overflow.
        let mut parent = self.parent.take();
        while let Some(node) = parent {
            match Rc::try_unwrap(node) {
                Ok(mut unique) => parent = unique.parent.take(),
                Err(_) => break,
            }
        }
    }
}

/// A retained path with immutable shared prefixes and a unique outer handle.
/// `iter` borrows segments; `to_vec` is an explicit temporary flat export.
/// Public mutation creates a new tail, preserving other cloned path snapshots.
pub struct PrefixPath {
    tail: Option<Rc<PathNode>>,
    len: usize,
}

impl PrefixPath {
    fn from_tail(tail: Option<Rc<PathNode>>, len: usize) -> Self {
        count!(|c| {
            c.handles_created += 1;
            c.handles_live += 1;
        });
        Self { tail, len }
    }

    /// Construct without a pool. Use `Engine::retained_path` to share prefixes
    /// across independently constructed container paths in one engine.
    pub fn from_segments(segments: &[Seg]) -> Self {
        let mut path = Self::default();
        for segment in segments {
            path.push(segment.clone());
        }
        path
    }

    /// Number of canonical segments.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether this path has no segments.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Borrow the root segment, walking its ancestry.
    pub fn first(&self) -> Option<&Seg> {
        self.get(0)
    }

    /// Borrow the tail segment in constant time.
    pub fn last(&self) -> Option<&Seg> {
        self.tail.as_deref().map(|node| &node.segment)
    }

    /// Indexed lookup walks backwards from the tail. Sequential consumers
    /// should use `iter`, which constructs its borrowed traversal once.
    pub fn get(&self, index: usize) -> Option<&Seg> {
        if index >= self.len {
            return None;
        }
        let mut node = self.tail.as_deref()?;
        for _ in 0..self.len - index - 1 {
            node = node.parent.as_deref()?;
        }
        Some(&node.segment)
    }

    /// Borrow segments in canonical order; deep paths use temporary references.
    pub fn iter(&self) -> PrefixPathIter<'_> {
        PrefixPathIter::new(self)
    }

    /// Format through the same canonical spelling as `path_str`, borrowing
    /// segments and retaining no flat vector.
    pub fn format(&self, rel_root: Option<&str>) -> String {
        super::path_str_iter(self.iter(), rel_root)
    }

    /// Export an owned flat path and record the explicit conversion.
    pub fn to_vec(&self) -> SegPath {
        count!(|c| {
            c.flat_exports += 1;
            c.flat_export_segments += self.len as u64;
        });
        // One exact requested buffer, O(depth), no retained flat cache and no
        // iterator spill allocation even for unusually deep public API paths.
        let mut result = Vec::with_capacity(self.len);
        let mut node = self.tail.as_deref();
        while let Some(current) = node {
            result.push(current.segment.clone());
            node = current.parent.as_deref();
        }
        result.reverse();
        result
    }

    /// Append without changing any shared prefix node.
    pub fn push(&mut self, segment: Seg) {
        self.tail = Some(PathNode::new(self.tail.take(), segment));
        self.len += 1;
    }

    /// Remove the tail without modifying any shared prefix node.
    pub fn pop(&mut self) -> Option<Seg> {
        let tail = self.tail.take()?;
        let result = tail.segment.clone();
        self.tail = tail.parent.clone();
        self.len -= 1;
        Some(result)
    }

    /// Release this body's ancestry without changing other snapshots.
    pub fn clear(&mut self) {
        self.tail = None;
        self.len = 0;
    }

    /// Borrowed ancestry identity for an external diagnostic only. This is not
    /// a stable language identity, and does not identify a reference snapshot.
    pub fn prefix_node_ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.prefix_nodes().map(|(id, _)| id)
    }

    /// Tail-to-root borrowed nodes for a deduplicating owner census. The tuple
    /// exposes each node's segment owner without cloning or flattening paths.
    pub fn prefix_nodes(&self) -> impl Iterator<Item = (usize, &Seg)> + '_ {
        std::iter::successors(self.tail.as_deref(), |node| node.parent.as_deref())
            .map(|node| (node as *const PathNode as usize, &node.segment))
    }
}

impl Default for PrefixPath {
    fn default() -> Self {
        Self::from_tail(None, 0)
    }
}

impl Clone for PrefixPath {
    fn clone(&self) -> Self {
        Self::from_tail(self.tail.clone(), self.len)
    }
}

#[cfg(feature = "runtime-diagnostics")]
impl Drop for PrefixPath {
    fn drop(&mut self) {
        count!(|c| c.handles_live -= 1);
    }
}

impl From<SegPath> for PrefixPath {
    fn from(segments: SegPath) -> Self {
        let mut path = Self::default();
        for segment in segments {
            path.push(segment);
        }
        path
    }
}

impl fmt::Debug for PrefixPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl PartialEq for PrefixPath {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl Eq for PrefixPath {}

/// Forward/backward borrowed traversal. Depth <= 16 allocates nothing; deeper
/// paths allocate a temporary vector of node references, never cloned Segs.
pub struct PrefixPathIter<'a> {
    inline: [Option<&'a PathNode>; INLINE_ITER_DEPTH],
    spill: Option<Vec<&'a PathNode>>,
    front: usize,
    back: usize,
}

impl<'a> PrefixPathIter<'a> {
    fn new(path: &'a PrefixPath) -> Self {
        let mut result = Self {
            inline: [None; INLINE_ITER_DEPTH],
            spill: None,
            front: 0,
            back: path.len,
        };
        if path.len > INLINE_ITER_DEPTH {
            count!(|c| c.iterator_spills += 1);
            result.spill = Some(Vec::with_capacity(path.len));
        }
        let mut node = path.tail.as_deref();
        let mut index = 0;
        while let Some(current) = node {
            if let Some(spill) = &mut result.spill {
                spill.push(current);
            } else {
                result.inline[index] = Some(current);
            }
            index += 1;
            node = current.parent.as_deref();
        }
        result
    }

    fn reverse_at(&self, index: usize) -> &'a Seg {
        match &self.spill {
            Some(spill) => &spill[index].segment,
            None => {
                &self.inline[index]
                    .expect("complete prefix ancestry")
                    .segment
            }
        }
    }
}

impl<'a> Iterator for PrefixPathIter<'a> {
    type Item = &'a Seg;

    fn next(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        self.back -= 1;
        Some(self.reverse_at(self.back))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.back - self.front;
        (remaining, Some(remaining))
    }
}

impl DoubleEndedIterator for PrefixPathIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        let index = self.front;
        self.front += 1;
        Some(self.reverse_at(index))
    }
}

impl ExactSizeIterator for PrefixPathIter<'_> {}
impl std::iter::FusedIterator for PrefixPathIter<'_> {}

impl<'a> IntoIterator for &'a PrefixPath {
    type Item = &'a Seg;
    type IntoIter = PrefixPathIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// A bounded accelerator for sharing existing prefixes, not a semantic cache.
/// Weak entries do not retain path bodies; clearing never invalidates a path.
#[derive(Default)]
pub struct PrefixPathPool {
    entries: FxHashMap<(usize, Seg), Weak<PathNode>>,
    // Never keep an outer path or a strong node owner in this accelerator.
    cursor_tail: Weak<PathNode>,
    cursor_len: usize,
}

/// Explicit bounded-pool occupancy and weak-storage diagnostic.
#[derive(Clone, Copy, Debug)]
pub struct PrefixPathPoolStats {
    /// Zero or one additional weak node references held by the ancestry cursor.
    /// This can refer to the same allocation as a hash-table entry.
    pub cursor_weak_entries: usize,
    /// Cursor weak references whose node body has died. Not an additional unique
    /// allocation count: the hash table can also hold the same dead allocation.
    pub cursor_dead_entries: usize,
    /// Cached path depth, zero when empty and never greater than 16.
    pub cursor_depth: usize,
    /// Weak entries currently retained by this pool.
    pub entries: usize,
    /// HashMap usable capacity, not allocator bytes or physical bucket count.
    pub usable_capacity: usize,
    /// Maximum entries before the next insertion clears the pool.
    pub entry_limit: usize,
    /// Weak entries whose node body has died but allocation remains reserved.
    pub dead_entries: usize,
}

impl PrefixPathPool {
    /// Retain a distinct outer path handle with any locally shared prefixes.
    pub fn retain(&mut self, segments: &[Seg]) -> Rc<PrefixPath> {
        Rc::new(self.retain_value(segments))
    }

    /// Retain a path value with shared prefixes, without an outer allocation.
    /// Cloning or mutating the returned handle leaves other handles independent.
    pub fn retain_value(&mut self, segments: &[Seg]) -> PrefixPath {
        count!(|c| {
            c.retain_calls += 1;
            c.input_segments += segments.len() as u64;
        });
        let mut parent: Option<Rc<PathNode>> = None;
        let mut reused = 0;
        if segments.len() > INLINE_ITER_DEPTH {
            count!(|c| c.cursor_deep_fallbacks += 1);
        } else if !segments.is_empty() && self.cursor_len != 0 {
            count!(|c| c.cursor_attempts += 1);
            if let Some(tail) = self.cursor_tail.upgrade() {
                // The temporary tail owner keeps these borrowed Rc references
                // valid. No flattened Seg vector or persistent strong owner.
                let mut ancestors: [Option<&Rc<PathNode>>; INLINE_ITER_DEPTH] =
                    [None; INLINE_ITER_DEPTH];
                let mut node = Some(&tail);
                let mut index = self.cursor_len;
                while let Some(current) = node {
                    index -= 1;
                    ancestors[index] = Some(current);
                    node = current.parent.as_ref();
                }
                for (segment, ancestor) in segments.iter().zip(&ancestors[..self.cursor_len]) {
                    if segment != &ancestor.expect("complete cursor ancestry").segment {
                        break;
                    }
                    reused += 1;
                }
                if reused != 0 {
                    parent = Some(Rc::clone(
                        ancestors[reused - 1].expect("matched cursor prefix"),
                    ));
                    count!(|c| {
                        c.cursor_hits += 1;
                        c.reused_prefix_segments += reused as u64;
                    });
                }
            }
        }
        for segment in &segments[reused..] {
            let parent_id = parent.as_ref().map_or(0, |node| Rc::as_ptr(node) as usize);
            let key = (parent_id, segment.clone());
            let node = match self.entries.get(&key).and_then(Weak::upgrade) {
                Some(node) => {
                    // A successful upgrade pins this node's strong parent,
                    // hence its parent's address cannot have been recycled.
                    count!(|c| c.pool_hits += 1);
                    node
                }
                None => {
                    count!(|c| c.pool_misses += 1);
                    if self.entries.len() >= POOL_ENTRY_LIMIT {
                        self.entries.clear();
                        count!(|c| c.pool_clears += 1);
                    }
                    let node = PathNode::new(parent.clone(), segment.clone());
                    self.entries.insert(key, Rc::downgrade(&node));
                    node
                }
            };
            parent = Some(node);
        }
        if !segments.is_empty() && segments.len() <= INLINE_ITER_DEPTH {
            self.cursor_tail = Rc::downgrade(parent.as_ref().expect("nonempty retained path"));
            self.cursor_len = segments.len();
        } else {
            self.cursor_tail = Weak::new();
            self.cursor_len = 0;
        }
        // Never intern the outer Rc: snapshot reference identity is a separate
        // concern even if the representation is later adopted for references.
        PrefixPath::from_tail(parent, segments.len())
    }

    /// O(entry count), explicitly requested diagnostics; absent from binding.
    pub fn stats(&self) -> PrefixPathPoolStats {
        PrefixPathPoolStats {
            cursor_weak_entries: usize::from(self.cursor_len != 0),
            cursor_dead_entries: usize::from(
                self.cursor_len != 0 && self.cursor_tail.strong_count() == 0,
            ),
            cursor_depth: self.cursor_len,
            entries: self.entries.len(),
            usable_capacity: self.entries.capacity(),
            entry_limit: POOL_ENTRY_LIMIT,
            dead_entries: self
                .entries
                .values()
                .filter(|node| node.strong_count() == 0)
                .count(),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/private/prefix_path_test.rs"]
mod tests;
