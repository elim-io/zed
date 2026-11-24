//! Accessibility support, provided by [AccessKit][accesskit].
//!
//! There are user-facing guide-level docs [here](crate::_accessibility).
//!
//! ## Architecture
//!
//! ```text
//!                              ┌────────────────────────────────┐   ┌─────────────────────┐
//!                           ┌─▶│ AccessKit Adapter (MacOS)      │◀─▶│ MacOS System APIs   │
//!                           │  └────────────────────────────────┘   └─────────────────────┘
//!                           │
//! ┌──────┐   ┌───────────┐  │  ┌────────────────────────────────┐   ┌─────────────────────┐
//! │ GPUI │◀─▶│ AccessKit │◀─┼─▶│ AccessKit Adapter (Windows)    │◀─▶│ Windows System APIs │
//! └──────┘   └───────────┘  │  └────────────────────────────────┘   └─────────────────────┘
//!                           │
//!                           │  ┌────────────────────────────────┐   ┌─────────────────────┐
//!                           └─▶│ AccessKit Adapter (Linux)      │◀─▶│ dbus                │
//!                              └────────────────────────────────┘   └─────────────────────┘
//! ```
//!
//! In order for GPUI apps to be usable for people using assistive technology,
//! we must do a few things:
//! - Inform the system when the UI changes meaningfully. This includes:
//!   - Reporting new/removed/changed UI elements
//!   - *Not* reporting irrelevant UI changes, e.g. an invisible `div()` being
//!     added.
//!   - Reporting the appearance and capabilities of each UI element. For example:
//!     - What does this piece of text say?
//!     - How far along is this progress bar?
//!     - Can this node be focused?
//!     - Can this node have a value directly assigned? (e.g. a slider)
//! - Allowing the system to interact with the UI by dispatching actions to
//!   nodes. Note that AccessKit has its own [`Action`] type, which is not the
//!   [`crate::Action`] trait.
//! - Activate and deactivate accessibility features when requested by the
//!   system.
//!
//! Activating and deactivating at the right time is trivial, so I won't go into
//! detail here. The other two are almost orthogonal in implementation.
//!
//! The state for both lives in the [`A11y`] struct in this module.
//!
//! ### Reporting UI changes
//!
//! Every frame, we build a [`TreeUpdate`] and send it to the platform-specific
//! adapter. A [`TreeUpdate`] is a representation of a subset of the UI tree.
//! When the adapter receives the update, it diffs it against the previous
//! update, and calls platform-specific APIs to inform screen readers about the
//! changes. Nodes may have been created, destroyed, or updated.
//!
//! Each node has an ID, and this ID *should* be stable across frames. If a
//! node's ID changes, then, from AccessKit's point of view, it is a different
//! node.
//!
//! We derive the node ID from the [`GlobalElementId`] in
//! [`GlobalElementId::accesskit_node_id`]. Nodes without [`GlobalElementId`]s
//! cannot produce an AccessKit [`NodeId`], and so are not included in the
//! accessibility tree. We try to warn when using accessibility APIs on
//! [`div()`] without setting an ID.
//!
//! This all happens in [`Drawable::prepaint`]. The [`A11y`] struct maintains a
//! stack of nodes during prepainting, which we can use to calculate the
//! [`NodeId`]s, and record parent-child relationships. Once all [`Element`]s in
//! a frame have been prepainted, we send the resulting [`TreeUpdate`] object to
//! the adapter and the screen reader can announce the changes.
//!
//! ### Responding to actions
//!  
//! On adapter creation, we provide a callback to the adapter, which can be used
//! to dispatch actions. This callback forwards to [`A11y::action_listeners`], a
//! mapping from [`NodeId`]s to action handlers (basically just `Box<dyn
//! Fn()>`).
//!
//! This is populated in:
//! - [`Window::on_a11y_action`], which is called by:
//! - [`Interactivity::paint`], which is called by:
//! - [`InteractiveElement::on_a11y_action`], which is a public-facing API
//!
//! These are cleared at the start of a frame, and re-populated during painting.
//!
//! [`Element`]: crate::Element
//! [`GlobalElementId`]: crate::GlobalElementId
//! [`div()`]: crate::div
//! [`Interactivity::paint`]: crate::Interactivity::paint
//! [`InteractiveElement::on_a11y_action`]: crate::InteractiveElement::on_a11y_action
//! [`NodeId`]: accesskit::NodeId
//! [`Drawable::prepaint`]: crate::Drawable::prepaint

use crate::{App, Bounds, FocusId, Pixels, Window};
use accesskit::{Action, NodeId, Role, TextPosition, TextSelection, TreeUpdate};
use collections::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// The fixed AccessKit node ID used for the root of every window's a11y tree.
pub(crate) const ROOT_NODE_ID: NodeId = NodeId(0);

/// The root alone: a window's tree before its first frame was built.
pub(crate) fn root_alone() -> TreeUpdate {
    TreeUpdate {
        nodes: vec![(ROOT_NODE_ID, accesskit::Node::new(accesskit::Role::Window))],
        tree: Some(accesskit::Tree::new(ROOT_NODE_ID)),
        tree_id: accesskit::TreeId::ROOT,
        focus: ROOT_NODE_ID,
    }
}

/// A window's accessibility state shared with its platform adapter's callbacks (which may run on another thread).
///
/// An adapter asks for a window's tree once, at a client's first read of it, and must answer at once. A window that
/// builds no tree until then answers with its root alone and sends the tree a frame later, so a client that reads once
/// (a script's `find`, System Events) sees an empty window. So once a client has read any window of the app (`seen`,
/// one flag per `App`), every window opened later builds its tree from its first frame and holds it (`held`) until its
/// adapter asks: the first read gets the window whole. A window opened before any client read still answers its root
/// alone the first time; a later frame sends the tree, as before.
#[derive(Clone)]
pub(crate) struct Shared {
    /// Whether a11y features have been requested for this window (by its adapter, or forced by a test).
    pub(crate) active: Arc<AtomicBool>,
    /// An AX client has read a window of this app.
    seen: Arc<AtomicBool>,
    first: Arc<parking_lot::Mutex<FirstAsk>>,
}

#[derive(Default)]
struct FirstAsk {
    /// The adapter asked for this window's tree.
    asked: bool,
    /// The last tree built before it asked.
    held: Option<TreeUpdate>,
}

impl Shared {
    /// A new window's: active from its first frame when a client has read another window of the app.
    pub(crate) fn new(seen: Arc<AtomicBool>) -> Self {
        Self {
            active: Arc::new(AtomicBool::new(seen.load(Ordering::SeqCst))),
            seen,
            first: Arc::default(),
        }
    }

    /// The adapter's first request (a client reads this window): the window builds its tree from now on, every window
    /// the app opens later from its first frame, and the answer is the tree held, else the root alone.
    pub(crate) fn activate(&self) -> TreeUpdate {
        self.seen.store(true, Ordering::SeqCst);
        self.active.store(true, Ordering::SeqCst);
        let mut first = self.first.lock();
        first.asked = true;
        first.held.take().unwrap_or_else(root_alone)
    }

    /// The update to send to the adapter, or none while it has not asked (the tree is held for its first request; an
    /// adapter that has not asked drops an update).
    pub(crate) fn hold(&self, update: TreeUpdate) -> Option<TreeUpdate> {
        let mut first = self.first.lock();
        if first.asked {
            return Some(update);
        }
        first.held = Some(update);
        None
    }
}

/// One shown row of a text a reader can select in, for [`Window::a11y_text`]: published as AccessKit text runs (a row
/// of more than 255 characters as several, linked on its line).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct A11yTextRow {
    /// The byte offset of the row's first character in the whole text.
    pub start: usize,
    /// The row's words. The last row of a line that a line break ends carries it ("\n", one character).
    pub text: String,
    /// Each character's (a grapheme cluster's) length in bytes; their sum is `text`'s length.
    pub lengths: Vec<u8>,
    /// Each character's left edge from the row's left and its advance, in window pixels; empty where the row was not
    /// laid out (a reader then gets no bounds for it).
    pub positions: Vec<f32>,
    /// Each character's advance, as `positions`.
    pub widths: Vec<f32>,
    /// The character index of each word's start, ascending.
    pub word_starts: Vec<usize>,
    /// The row's box in the window.
    pub bounds: Bounds<Pixels>,
}

/// The most characters one AccessKit text run holds (its word starts are `u8` indices).
const RUN_MAX: usize = 255;

/// The node id of the `index`th text run under `parent`: stable from frame to frame, apart from every element's.
fn run_id(parent: NodeId, index: usize) -> NodeId {
    NodeId((parent.0 ^ 0x7e57_0000_0000_0000).wrapping_add((index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)))
}

/// Where `off` (a byte offset of the whole text) falls among `rows`: the row and the character index in it. At a soft
/// wrap the offset is the next row's start; at a line's end it is its line break; past the end, the last row's end.
fn row_position(rows: &[A11yTextRow], off: usize) -> Option<(usize, usize)> {
    let r = rows.partition_point(|row| row.start <= off).checked_sub(1).unwrap_or(0);
    let row = rows.get(r)?;
    let (mut at, mut i) = (row.start, 0);
    while i < row.lengths.len() && at + usize::from(row.lengths[i]) <= off {
        at += usize::from(row.lengths[i]);
        i += 1;
    }
    Some((r, i))
}

/// A listener for an accessibility action on a specific node.
pub(crate) type A11yActionListener =
    Box<dyn FnMut(Option<&accesskit::ActionData>, &mut Window, &mut App) + 'static>;

/// Per-window accessibility state.
///
/// Manages the AccessKit tree that is built each frame and the mappings
/// needed to dispatch incoming action requests back to the right elements.
pub(crate) struct A11y {
    /// Whether accessibility has been [forcibly disabled] for this window.
    ///
    /// [forcibly disabled]: crate::Application::new_inaccessible
    force_disabled: bool,
    /// Whether a11y features have been requested by the system (`shared.active`), and the tree held for the adapter's
    /// first request.
    ///
    /// Updated by AccessKit using callbacks provided to the adapter. Can change
    /// halfway through a frame.
    pub(crate) shared: Shared,
    /// Whether a11y features are active for *this specific frame*.
    ///
    /// At the start of each frame, we load [`Self::shared`] (using
    /// [`Self::sync_active_flag`]) and use this to determine whether we
    /// should construct a [`TreeUpdate`] for this frame. It's important that
    /// this value is stable within a frame, because the builder API exposed by
    /// this type maintains a stack of nodes and each must be pushed and popped
    /// exactly once.
    ///
    /// At the end of the frame, we re-call [`Self::sync_active_flag`] to
    /// determine whether we should actually send the finished [`TreeUpdate`].
    active_this_frame: bool,
    pub(crate) nodes: A11yNodeBuilder,
    pub(crate) focus_ids: FxHashMap<NodeId, FocusId>,
    pub(crate) node_bounds: FxHashMap<NodeId, Bounds<Pixels>>,
    pub(crate) action_listeners: FxHashMap<NodeId, Vec<(Action, A11yActionListener)>>,
    /// The last frame's whole tree, for tests and tree dumps.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) last_tree: Option<TreeUpdate>,
    /// The global ids of the nodes dropped as duplicates in the last frame.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) duplicates: Vec<String>,
}

impl A11y {
    pub(crate) fn new(shared: Shared, force_disabled: bool) -> Self {
        Self {
            force_disabled,
            shared,
            active_this_frame: false,
            nodes: A11yNodeBuilder::new(),
            focus_ids: FxHashMap::default(),
            node_bounds: FxHashMap::default(),
            action_listeners: FxHashMap::default(),
            #[cfg(any(test, feature = "test-support"))]
            last_tree: None,
            #[cfg(any(test, feature = "test-support"))]
            duplicates: Vec::new(),
        }
    }

    /// Ensures that [`Self::is_active`] returns up to date information.
    ///
    /// See the docs for [`Self::shared`] and [`Self::active_this_frame`]
    /// for more commentary.
    pub(crate) fn sync_active_flag(&mut self) {
        self.active_this_frame = !self.force_disabled && self.shared.active.load(Ordering::SeqCst);
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active_this_frame
    }

    /// Clear per-frame state and push the root node to start a new frame.
    pub(crate) fn begin_frame(&mut self) {
        self.focus_ids.clear();
        self.node_bounds.clear();
        self.action_listeners.clear();
        #[cfg(any(test, feature = "test-support"))]
        self.duplicates.clear();
        self.nodes.begin_frame();
    }

    /// Finalize the tree and produce a [`TreeUpdate`] for the platform adapter.
    pub(crate) fn end_frame(&mut self) -> TreeUpdate {
        self.nodes.finalize()
    }

    /// Sets the active flag with no adapter request (no tree is held for one).
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn force_active(&mut self) {
        self.shared.active.store(true, Ordering::SeqCst);
    }
}

pub(crate) struct A11yNodeBuilder {
    ids_stack: SmallVec<[NodeId; 16]>,
    nodes_stack: SmallVec<[accesskit::Node; 16]>,
    /// This is the exact type required by accesskit, so we can't just make it a
    /// `HashMap<NodeId, Node>` to remove the need for `seen_ids`
    all_nodes: Vec<(NodeId, accesskit::Node)>,
    seen_ids: FxHashSet<NodeId>,
    /// Duplicate ids already logged: each is logged once, not once a frame.
    logged_duplicates: FxHashSet<NodeId>,
    focus: NodeId,
}

impl A11yNodeBuilder {
    fn new() -> Self {
        Self {
            ids_stack: SmallVec::new(),
            nodes_stack: SmallVec::new(),
            all_nodes: Vec::new(),
            seen_ids: FxHashSet::default(),
            logged_duplicates: FxHashSet::default(),
            focus: ROOT_NODE_ID,
        }
    }

    /// Push a new node onto the stack. It becomes a child of the current
    /// top-of-stack node.
    ///
    /// Returns `true` if the node was successfully pushed.
    pub(crate) fn push(&mut self, id: NodeId, node: accesskit::Node) -> bool {
        debug_assert!(!self.ids_stack.is_empty(), "push called before push_root");

        if !self.seen_ids.insert(id) {
            if self.logged_duplicates.insert(id) {
                log::error!(
                    "a11y: duplicate node id {id:?}: two elements share one global id; the second is dropped from the tree"
                );
            }
            // We need to return `false` here because inserting a duplicate
            // node will cause a panic in accesskit
            return false;
        }

        if let Some(parent) = self.nodes_stack.last_mut() {
            parent.push_child(id);
        }
        self.ids_stack.push(id);
        self.nodes_stack.push(node);
        true
    }

    /// Pop the current node off the stack and finalize it into the all_nodes
    /// list.
    pub(crate) fn pop(&mut self) {
        debug_assert!(self.ids_stack.len() > 1, "pop would remove the root node");

        if let (Some(id), Some(node)) = (self.ids_stack.pop(), self.nodes_stack.pop()) {
            self.all_nodes.push((id, node));
        }
    }

    /// Push the root node to start a new frame.
    fn begin_frame(&mut self) {
        self.all_nodes.clear();
        self.ids_stack.clear();
        self.nodes_stack.clear();
        self.seen_ids.clear();
        let root_node = accesskit::Node::new(accesskit::Role::Window);

        self.ids_stack.push(ROOT_NODE_ID);
        self.nodes_stack.push(root_node);
        self.focus = ROOT_NODE_ID;
    }

    /// Returns whether a node with the given ID has been pushed in this frame.
    pub(crate) fn has_node(&self, id: NodeId) -> bool {
        id == ROOT_NODE_ID || self.seen_ids.contains(&id)
    }

    /// The text runs of `rows` under the node being built, and `selection` (anchor, head: byte offsets) as its text
    /// selection ([`Window::a11y_text`]). Nothing under the root, a password's node, or rows whose lengths do not
    /// cover their words.
    pub(crate) fn text(&mut self, rows: &[A11yTextRow], selection: Option<(usize, usize)>, scale: f32) {
        let Some(&parent) = self.ids_stack.last() else {
            return;
        };
        let Some(node) = self.nodes_stack.last_mut() else {
            return;
        };
        if parent == ROOT_NODE_ID || node.role() == Role::PasswordInput {
            return;
        }
        if rows.iter().any(|r| {
            r.lengths.iter().map(|&l| usize::from(l)).sum::<usize>() != r.text.len()
                || r.lengths.iter().any(|&l| l == 0)
        }) {
            log::error!("a11y: text rows whose character lengths do not cover their words; no text runs published");
            return;
        }
        let px = |p: Pixels| f64::from(p.0 * scale);
        // (first run, runs) of each row
        let mut spans = Vec::with_capacity(rows.len());
        let mut runs: Vec<(NodeId, accesskit::Node)> = Vec::with_capacity(rows.len());
        for row in rows {
            let n = row.lengths.len();
            let laid = row.positions.len() == n && row.widths.len() == n;
            let first = runs.len();
            let (mut a, mut byte) = (0, 0);
            loop {
                let b = (a + RUN_MAX).min(n);
                let bytes: usize = row.lengths[a..b].iter().map(|&l| usize::from(l)).sum();
                let id = run_id(parent, runs.len());
                let mut run = accesskit::Node::new(Role::TextRun);
                run.set_value(&row.text[byte..byte + bytes]);
                run.set_character_lengths(&row.lengths[a..b]);
                let words: Vec<u8> = row
                    .word_starts
                    .iter()
                    .filter(|&&w| w >= a && w < b)
                    .filter_map(|&w| u8::try_from(w - a).ok())
                    .collect();
                run.set_word_starts(words);
                run.set_text_direction(accesskit::TextDirection::LeftToRight);
                if laid {
                    let left = if a < b { row.positions[a] } else { 0. };
                    let right = if a < b { row.positions[b - 1] + row.widths[b - 1] } else { left };
                    let o = row.bounds.origin;
                    run.set_bounds(accesskit::Rect {
                        x0: px(o.x + Pixels(left)),
                        y0: px(o.y),
                        x1: px(o.x + Pixels(right)),
                        y1: px(o.y + row.bounds.size.height),
                    });
                    run.set_character_positions(
                        row.positions[a..b].iter().map(|p| (p - left) * scale).collect::<Vec<_>>(),
                    );
                    run.set_character_widths(row.widths[a..b].iter().map(|w| w * scale).collect::<Vec<_>>());
                }
                if a > 0 {
                    let prev = run_id(parent, runs.len() - 1);
                    run.set_previous_on_line(prev);
                    if let Some((_, p)) = runs.last_mut() {
                        p.set_next_on_line(id);
                    }
                }
                runs.push((id, run));
                byte += bytes;
                a = b;
                if a >= n {
                    break;
                }
            }
            spans.push((first, runs.len() - first));
        }
        let position = |off: usize| {
            let (r, i) = row_position(rows, off)?;
            let (first, count) = spans[r];
            let k = (i / RUN_MAX).min(count - 1);
            Some(TextPosition {
                node: run_id(parent, first + k),
                character_index: i - k * RUN_MAX,
            })
        };
        if let Some((anchor, head)) = selection
            && let (Some(anchor), Some(focus)) = (position(anchor), position(head))
        {
            node.set_text_selection(TextSelection { anchor, focus });
        }
        for (id, run) in runs {
            if self.push(id, run) {
                self.pop();
            }
        }
    }

    /// Set the focused node for this frame. A second call in one frame wins.
    pub(crate) fn set_focus(&mut self, id: NodeId) {
        self.focus = id;
    }

    fn finalize(&mut self) -> TreeUpdate {
        // Stack should contain only the root node
        debug_assert_eq!(self.ids_stack.len(), 1);
        debug_assert_eq!(self.ids_stack[0], ROOT_NODE_ID);

        if self.ids_stack.len() != 1 {
            log::error!(
                "a11y: Stack imbalance at end of frame: expected 1 (root), got {}. \
                 Some elements may have pushed without popping.",
                self.ids_stack.len()
            );
        }

        // Pop remaining nodes (should just be the root).
        while !self.ids_stack.is_empty() {
            if let (Some(id), Some(node)) = (self.ids_stack.pop(), self.nodes_stack.pop()) {
                self.all_nodes.push((id, node));
            }
        }

        let nodes = std::mem::take(&mut self.all_nodes);
        let update = TreeUpdate {
            nodes,
            tree: Some(accesskit::Tree::new(ROOT_NODE_ID)),
            tree_id: accesskit::TreeId::ROOT,
            focus: self.focus,
        };

        Self::repair_tree_update(update)
    }

    /// Accesskit panics on invalid [`TreeUpdate`]s. This function defensively
    /// checks invariants that accesskit panics on, and tries to fix them.
    fn repair_tree_update(mut update: TreeUpdate) -> TreeUpdate {
        let node_ids: FxHashSet<NodeId> = update.nodes.iter().map(|(id, _)| *id).collect();

        // Focus must point to a node in the tree.
        if !node_ids.contains(&update.focus) {
            log::error!(
                "a11y: Focused node {:?} is not in the tree ({} nodes). \
                 Falling back to root. This is a bug in the a11y tree builder.",
                update.focus,
                update.nodes.len()
            );
            update.focus = ROOT_NODE_ID;
        }

        // Every child reference must point to a node in the update.
        for (id, node) in &mut update.nodes {
            let has_invalid_child = node
                .children()
                .iter()
                .any(|child_id| !node_ids.contains(child_id));
            if has_invalid_child {
                let children = node.children();
                let invalid_count = children
                    .iter()
                    .filter(|child_id| !node_ids.contains(child_id))
                    .count();
                log::error!(
                    "a11y: Node {:?} references {} children not present in the tree. \
                     Stripping invalid child references.",
                    id,
                    invalid_count
                );
                let valid: Vec<NodeId> = children
                    .iter()
                    .copied()
                    .filter(|child_id| node_ids.contains(child_id))
                    .collect();
                node.set_children(valid);
            }
        }

        update
    }
}

#[cfg(test)]
#[path = "a11y_tests.rs"]
mod tests;
