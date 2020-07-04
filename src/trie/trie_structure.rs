use super::nibble::Nibble;

use core::{convert::TryFrom as _, iter};
use either::Either;
use slab::Slab;

/// Stores the structure of a trie, including branch nodes that have no storage value.
///
/// The `TUd` parameter is a user data stored in each node.
///
/// This struct doesn't represent a complete trie. It only manages the structure of the trie, and
/// the storage values have to be maintained in parallel of this.
#[derive(Debug)]
pub struct TrieStructure<TUd> {
    /// List of nodes. Using a [`Slab`] guarantees that the node indices don't change.
    nodes: Slab<Node<TUd>>,
    /// Index of the root node within [`nodes`]. `None` if the trie is empty.
    root_index: Option<usize>,
}

#[derive(Debug)]
struct Node<TUd> {
    /// Index of the parent within [`TrieStructure::nodes`]. `None` if this is the root.
    parent: Option<(usize, Nibble)>,
    /// Partial key of the node. Portion to add to the values in `parent` to obtain the full key.
    partial_key: Vec<Nibble>, // TODO: SmallVec?
    /// Indices of the children within [`TrieStructure::nodes`].
    children: [Option<usize>; 16],
    has_storage_value: bool,
    /// User data associated to the node.
    user_data: TUd,
}

impl<TUd> TrieStructure<TUd> {
    /// Builds a new empty trie.
    pub fn new() -> Self {
        TrieStructure {
            nodes: Slab::new(),
            root_index: None,
        }
    }

    /// Builds a new empty trie with a capacity for the given number of nodes.
    pub fn with_capacity(capacity: usize) -> Self {
        TrieStructure {
            nodes: Slab::with_capacity(capacity),
            root_index: None,
        }
    }

    /// Reduces the capacity of the trie as much as possible.
    pub fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit();
    }

    /// Returns the root node of the trie, or `None` if the trie is empty.
    pub fn root_node(&mut self) -> Option<NodeAccess<TUd>> {
        Some(self.node_by_index(self.root_index?).unwrap())
    }

    /// Returns a [`Entry`] with the given node.
    pub fn node<'a>(&'a mut self, key: &'a [Nibble]) -> Entry<'a, TUd> {
        match self.existing_node_inner(key.iter().cloned()) {
            ExistingNodeInnerResult::Found {
                node_index,
                has_storage_value: true,
            } => Entry::Occupied(NodeAccess::Storage(StorageNodeAccess {
                trie: self,
                node_index,
            })),
            ExistingNodeInnerResult::Found {
                node_index,
                has_storage_value: false,
            } => Entry::Occupied(NodeAccess::Branch(BranchNodeAccess {
                trie: self,
                node_index,
            })),
            ExistingNodeInnerResult::NotFound { closest_ancestor } => Entry::Vacant(Vacant {
                trie: self,
                key,
                closest_ancestor,
            }),
        }
    }

    /// Returns the node with the given key, or `None` if no such node exists.
    pub fn existing_node(
        &mut self,
        mut key: impl Iterator<Item = Nibble>,
    ) -> Option<NodeAccess<TUd>> {
        if let ExistingNodeInnerResult::Found {
            node_index,
            has_storage_value,
        } = self.existing_node_inner(key)
        {
            Some(if has_storage_value {
                NodeAccess::Storage(StorageNodeAccess {
                    trie: self,
                    node_index,
                })
            } else {
                NodeAccess::Branch(BranchNodeAccess {
                    trie: self,
                    node_index,
                })
            })
        } else {
            None
        }
    }

    /// Inner implementation of [`TrieStructure::existing_node`]. Traverses the tree, trying to
    /// find a node whose key is `key`.
    fn existing_node_inner(
        &mut self,
        mut key: impl Iterator<Item = Nibble>,
    ) -> ExistingNodeInnerResult {
        let mut current_index = match self.root_index {
            Some(ri) => ri,
            None => {
                return ExistingNodeInnerResult::NotFound {
                    closest_ancestor: None,
                }
            }
        };
        debug_assert!(self.nodes.get(current_index).unwrap().parent.is_none());

        let mut closest_ancestor = None;

        loop {
            let current = self.nodes.get(current_index).unwrap();

            // First, we must remove `current`'s partial key from `key`, making sure that they
            // match.
            for nibble in current.partial_key.iter().cloned() {
                if key.next() != Some(nibble) {
                    return ExistingNodeInnerResult::NotFound { closest_ancestor };
                }
            }

            // At this point, the tree traversal cursor (the `key` iterator) exactly matches
            // `current`.
            // If `key.next()` is `Some`, put it in `child_index`, otherwise return successfully.
            let child_index = match key.next() {
                Some(n) => n,
                None => {
                    return ExistingNodeInnerResult::Found {
                        node_index: current_index,
                        has_storage_value: current.has_storage_value,
                    }
                }
            };

            closest_ancestor = Some(current_index);

            if let Some(next_index) = current.children[usize::from(u8::from(child_index))] {
                current_index = next_index;
            } else {
                return ExistingNodeInnerResult::NotFound { closest_ancestor };
            }
        }
    }

    /// Internal function. Returns the [`NodeAccess`] of the node at the given index.
    fn node_by_index(&mut self, node_index: usize) -> Option<NodeAccess<TUd>> {
        if self.nodes.get(node_index)?.has_storage_value {
            Some(NodeAccess::Storage(StorageNodeAccess {
                trie: self,
                node_index,
            }))
        } else {
            Some(NodeAccess::Branch(BranchNodeAccess {
                trie: self,
                node_index,
            }))
        }
    }

    /// Returns the full key of the node with the given index.
    fn node_full_key<'b>(&'b self, target: usize) -> impl Iterator<Item = Nibble> + 'b {
        self.node_path(target)
            .chain(iter::once(target))
            .flat_map(move |n| {
                let node = self.nodes.get(n).unwrap();
                let child_index = node.parent.into_iter().map(|p| p.1);
                let partial_key = node.partial_key.iter().cloned();
                child_index.chain(partial_key)
            })
    }

    /// Returns the indices of the nodes to traverse to reach `target`. The returned iterator
    /// does *not* include `target`. In other words, if `target` is the root node, this returns
    /// an empty iterator.
    fn node_path<'b>(&'b self, target: usize) -> impl Iterator<Item = usize> + 'b {
        debug_assert!(self.nodes.get(usize::max_value()).is_none());
        // First element is an invalid key, each successor is the last element of
        // `reverse_node_path(target)` that isn't equal to `current`.
        // Since the first element is invalid, we skip it.
        // Since `reverse_node_path` never produces `target`, we know that it also won't be
        // produced here.
        iter::successors(Some(usize::max_value()), move |&current| {
            self.reverse_node_path(target)
                .take_while(move |n| *n != current)
                .last()
        })
        .skip(1)
    }

    /// Returns the indices of the nodes starting from `target` towards the root node. The returned
    /// iterator does *not* include `target` but does include the root node if it is different
    /// from `target`. If `target` is the root node, this returns an empty iterator.
    fn reverse_node_path<'b>(&'b self, target: usize) -> impl Iterator<Item = usize> + 'b {
        // First element is `target`, each successor is `current.parent`.
        // Since `target` must explicitly not be included, we skip the first element.
        iter::successors(Some(target), move |current| {
            Some(self.nodes.get(*current).unwrap().parent?.0)
        })
        .skip(1)
    }
}

enum ExistingNodeInnerResult {
    Found {
        node_index: usize,
        has_storage_value: bool,
    },
    NotFound {
        /// Closest ancestor that actually exists.
        closest_ancestor: Option<usize>,
    },
}

/// Access to a entry for a potential node within the [`TrieStructure`].
pub enum Entry<'a, TUd> {
    /// There exists a node with this key.
    Occupied(NodeAccess<'a, TUd>),
    /// This entry is vacant.
    Vacant(Vacant<'a, TUd>),
}

impl<'a, TUd> Entry<'a, TUd> {
    /// Returns `Some` if `self` is an [`Entry::Vacant`].
    pub fn into_vacant(self) -> Option<Vacant<'a, TUd>> {
        match self {
            Entry::Vacant(e) => Some(e),
            _ => None,
        }
    }

    /// Returns `Some` if `self` is an [`Entry::Occupied`].
    pub fn into_occupied(self) -> Option<NodeAccess<'a, TUd>> {
        match self {
            Entry::Occupied(e) => Some(e),
            _ => None,
        }
    }
}

/// Access to a node within the [`TrieStructure`].
pub enum NodeAccess<'a, TUd> {
    Storage(StorageNodeAccess<'a, TUd>),
    Branch(BranchNodeAccess<'a, TUd>),
}

impl<'a, TUd> NodeAccess<'a, TUd> {
    /// Returns true if the node has a storage value associated to it.
    pub fn parent(self) -> Option<NodeAccess<'a, TUd>> {
        match self {
            NodeAccess::Storage(n) => n.parent(),
            NodeAccess::Branch(n) => n.parent(),
        }
    }

    /// Returns the child of this node given the given index.
    ///
    /// Returns back `self` if there is no such child at this index.
    pub fn child(self, index: Nibble) -> Result<NodeAccess<'a, TUd>, Self> {
        match self {
            NodeAccess::Storage(n) => n.child(index).map_err(NodeAccess::Storage),
            NodeAccess::Branch(n) => n.child(index).map_err(NodeAccess::Branch),
        }
    }

    /// Returns true if this node is the root node of the trie.
    pub fn is_root_node(&self) -> bool {
        match self {
            NodeAccess::Storage(n) => n.is_root_node(),
            NodeAccess::Branch(n) => n.is_root_node(),
        }
    }

    /// Returns the full key of the node.
    pub fn full_key<'b>(&'b self) -> impl Iterator<Item = Nibble> + 'b {
        match self {
            NodeAccess::Storage(n) => Either::Left(n.full_key()),
            NodeAccess::Branch(n) => Either::Right(n.full_key()),
        }
    }

    /// Returns the partial key of the node.
    pub fn partial_key<'b>(&'b self) -> impl ExactSizeIterator<Item = Nibble> + 'b {
        match self {
            NodeAccess::Storage(n) => Either::Left(n.partial_key()),
            NodeAccess::Branch(n) => Either::Right(n.partial_key()),
        }
    }

    /// Returns the user data stored in the node.
    pub fn user_data(&mut self) -> &mut TUd {
        match self {
            NodeAccess::Storage(n) => n.user_data(),
            NodeAccess::Branch(n) => n.user_data(),
        }
    }

    /// Returns true if the node has a storage value associated to it.
    pub fn has_storage_value(&self) -> bool {
        match self {
            NodeAccess::Storage(_) => true,
            NodeAccess::Branch(_) => false,
        }
    }
}

/// Access to a node within the [`TrieStructure`] that is known to have a storage value associated
/// to it.
pub struct StorageNodeAccess<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,
    node_index: usize,
}

impl<'a, TUd> StorageNodeAccess<'a, TUd> {
    /// Returns the parent of this node, or `None` if this is the root node.
    pub fn parent(self) -> Option<NodeAccess<'a, TUd>> {
        let parent_idx = self.trie.nodes.get(self.node_index).unwrap().parent?.0;
        if self.trie.nodes.get(parent_idx).unwrap().has_storage_value {
            Some(NodeAccess::Storage(StorageNodeAccess {
                trie: self.trie,
                node_index: parent_idx,
            }))
        } else {
            Some(NodeAccess::Branch(BranchNodeAccess {
                trie: self.trie,
                node_index: parent_idx,
            }))
        }
    }

    /// Returns the child of this node given the given index.
    ///
    /// Returns back `self` if there is no such child at this index.
    pub fn child(self, index: Nibble) -> Result<NodeAccess<'a, TUd>, Self> {
        let child_idx = match self.trie.nodes.get(self.node_index).unwrap().children
            [usize::from(u8::from(index))]
        {
            Some(c) => c,
            None => return Err(self),
        };

        if self.trie.nodes.get(child_idx).unwrap().has_storage_value {
            Ok(NodeAccess::Storage(StorageNodeAccess {
                trie: self.trie,
                node_index: child_idx,
            }))
        } else {
            Ok(NodeAccess::Branch(BranchNodeAccess {
                trie: self.trie,
                node_index: child_idx,
            }))
        }
    }

    /// Returns true if this node is the root node of the trie.
    pub fn is_root_node(&self) -> bool {
        self.trie.root_index == Some(self.node_index)
    }

    /// Returns the full key of the node.
    pub fn full_key<'b>(&'b self) -> impl Iterator<Item = Nibble> + 'b {
        self.trie.node_full_key(self.node_index)
    }

    /// Returns the partial key of the node.
    pub fn partial_key<'b>(&'b self) -> impl ExactSizeIterator<Item = Nibble> + 'b {
        self.trie
            .nodes
            .get(self.node_index)
            .unwrap()
            .partial_key
            .iter()
            .cloned()
    }

    /// Returns the user data associated to this node.
    pub fn user_data(&mut self) -> &mut TUd {
        &mut self.trie.nodes.get_mut(self.node_index).unwrap().user_data
    }

    /// Removes the storage value and returns what this changes in the trie structure.
    pub fn remove(self) -> Remove<'a, TUd> {
        // If the removed node has 2 or more children, then the node continues as a branch node.
        if self
            .trie
            .nodes
            .get(self.node_index)
            .unwrap()
            .children
            .iter()
            .filter(|c| c.is_some())
            .count()
            >= 2
        {
            return Remove::StorageToBranch(BranchNodeAccess {
                trie: self.trie,
                node_index: self.node_index,
            });
        }

        let removed_node = self.trie.nodes.remove(self.node_index);
        debug_assert!(removed_node.has_storage_value);

        // We already know from above that the removed node has only 0 or 1 children. Let's
        // determine which.
        let child_node_index: Option<usize> =
            removed_node.children.iter().filter_map(|c| *c).next();

        // If relevant, update our single child's parent to point to `removed_node`'s parent.
        if let Some(child_node_index) = child_node_index {
            let child = self.trie.nodes.get_mut(child_node_index).unwrap();
            debug_assert_eq!(child.parent.as_ref().unwrap().0, self.node_index);
            child.partial_key = {
                let mut nk = removed_node.partial_key;
                nk.push(child.parent.unwrap().1);
                nk.extend_from_slice(&child.partial_key);
                nk
            };
            child.parent = removed_node.parent;
        }

        // At this point, we're almost done with removing `removed_node` from `self.trie`. However
        // there is potentially another change to make: maybe `parent` has to be removed from the
        // trie as well.

        // `single_remove` is true if we can keep `parent` in the trie.
        let single_remove = if let Some((parent_index, child_index)) = removed_node.parent {
            // Update `removed_node`'s parent to point to the child.
            let parent = self.trie.nodes.get_mut(parent_index).unwrap();
            debug_assert_eq!(
                parent.children[usize::from(u8::from(child_index))],
                Some(self.node_index)
            );
            parent.children[usize::from(u8::from(child_index))] = child_node_index;

            // If `parent` does *not* need to be removed, we can return early.
            parent.has_storage_value || parent.children.iter().filter(|c| c.is_some()).count() >= 2
        } else {
            self.trie.root_index = child_node_index;
            true
        };

        // If we keep the parent in the trie, return early with a `SingleRemove`.
        if single_remove {
            return Remove::SingleRemove {
                user_data: removed_node.user_data,
                child: if let Some(child_node_index) = child_node_index {
                    let child_has_storage = self
                        .trie
                        .nodes
                        .get(child_node_index)
                        .unwrap()
                        .has_storage_value;
                    Some(if child_has_storage {
                        NodeAccess::Storage(StorageNodeAccess {
                            trie: self.trie,
                            node_index: child_node_index,
                        })
                    } else {
                        NodeAccess::Branch(BranchNodeAccess {
                            trie: self.trie,
                            node_index: child_node_index,
                        })
                    })
                } else {
                    None
                },
            };
        };

        // If we reach here, then parent has to be removed from the trie as well.
        let parent_index = removed_node.parent.unwrap().0;
        debug_assert!(child_node_index.is_none());
        let removed_branch = self.trie.nodes.remove(parent_index);
        debug_assert!(!removed_branch.has_storage_value);

        // We already know from above that the removed branch has exactly 1 sibling. Let's
        // determine which.
        let sibling_node_index: usize = removed_branch
            .children
            .iter()
            .filter_map(|c| *c)
            .next()
            .unwrap();

        // Update the sibling to point to the parent's parent.
        {
            let sibling = self.trie.nodes.get_mut(sibling_node_index).unwrap();
            debug_assert_eq!(sibling.parent.as_ref().unwrap().0, parent_index);
            sibling.partial_key = {
                let mut nk = removed_branch.partial_key;
                nk.push(sibling.parent.unwrap().1);
                nk.extend_from_slice(&sibling.partial_key);
                nk
            };
            sibling.parent = removed_branch.parent;
        }

        // Update the parent's parent to point to the sibling.
        if let Some((parent_parent_index, sibling_index)) = removed_branch.parent {
            // Update the parent's parent to point to the sibling.
            let parent_parent = self.trie.nodes.get_mut(parent_parent_index).unwrap();
            debug_assert_eq!(
                parent_parent.children[usize::from(u8::from(sibling_index))],
                Some(parent_index)
            );
            parent_parent.children[usize::from(u8::from(sibling_index))] = Some(sibling_node_index);
        } else {
            self.trie.root_index = Some(sibling_node_index);
        }

        // Success!
        Remove::BranchAlsoRemoved {
            sibling: {
                let sibling_has_storage = self
                    .trie
                    .nodes
                    .get(sibling_node_index)
                    .unwrap()
                    .has_storage_value;
                if sibling_has_storage {
                    NodeAccess::Storage(StorageNodeAccess {
                        trie: self.trie,
                        node_index: sibling_node_index,
                    })
                } else {
                    NodeAccess::Branch(BranchNodeAccess {
                        trie: self.trie,
                        node_index: sibling_node_index,
                    })
                }
            },
            storage_user_data: removed_node.user_data,
            branch_user_data: removed_branch.user_data,
        }
    }
}

/// Outcome of the removal of a storage value.
pub enum Remove<'a, TUd> {
    /// Removing the storage value didn't change the structure of the trie. Contains a
    /// [`BranchNodeAccess`] representing the same node as the [`StorageNodeAccess`] whose value
    /// got removed.
    StorageToBranch(BranchNodeAccess<'a, TUd>),

    /// Removing the storage value removed the node that contained the storage value. Apart from
    /// this removal, the structure of the trie didn't change.
    ///
    /// If it possible that the node that got removed had one single child, in which case this
    /// child's parent becomes the parent that the former node had.
    ///
    /// If `child` is `None`, then what happened is:
    ///
    /// ```ignore
    ///
    ///       Before                               After
    ///
    ///
    ///                +-+                         +-+
    ///           +--> +-+                         +-+
    ///           |
    ///           |
    ///           |
    ///           |
    ///          +-+
    ///          +-+ removed node
    ///
    /// ```
    ///
    ///
    /// If `child` is `Some`, then what happened is:
    ///
    /// ```ignore
    ///
    ///       Before                               After
    ///
    ///
    ///                +-+                                 +-+
    ///           +--> +-+                      +--------> +-+
    ///           |                             |
    ///           |                             |
    ///           |                             |
    ///           |                             |
    ///          +-+                            |
    ///     +--> +-+ removed node               |
    ///     |                                   |
    ///     |                                   |
    ///     |                                   |
    ///     |                                   |
    ///    +-+                                 +-+
    ///    +-+                                 +-+  `child`
    ///
    /// ```
    ///
    SingleRemove {
        /// Unique child that the removed node had. The parent and partial key of this child has
        /// been modified.
        child: Option<NodeAccess<'a, TUd>>,

        /// User data that was in the removed node.
        user_data: TUd,
    },

    /// Removing the storage value removed two nodes from the trie: the one that contained the
    /// storage value and its parent, which was a branch node.
    ///
    /// This can only happen if the removed node had no children and only one sibling.
    ///
    /// ```ignore
    ///
    ///             Before                        After
    ///
    ///
    ///               |                             |
    ///               |                             |
    ///              +-+                            |
    ///         +--> +-+ <--+                       +-----+
    ///         |           |                             |
    ///         |           |                             |
    ///        +-+         +-+                           +-+
    ///        +-+         +-+                           +-+
    ///
    ///  removed node                                 `sibling`
    ///
    /// ```
    ///
    BranchAlsoRemoved {
        /// Sibling of the removed node. The parent and partial key of this sibling have been
        /// modified.
        sibling: NodeAccess<'a, TUd>,

        /// User data that was in the removed storage node.
        storage_user_data: TUd,

        /// User data that was in the removed branch node (former parent of `storage_user_data`).
        branch_user_data: TUd,
    },
}

/// Access to a node within the [`TrieStructure`] that is known to not have any storage value
/// associated to it.
pub struct BranchNodeAccess<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,
    node_index: usize,
}

impl<'a, TUd> BranchNodeAccess<'a, TUd> {
    /// Returns the parent of this node, or `None` if this is the root node.
    pub fn parent(self) -> Option<NodeAccess<'a, TUd>> {
        let parent_idx = self.trie.nodes.get(self.node_index).unwrap().parent?.0;
        if self.trie.nodes.get(parent_idx).unwrap().has_storage_value {
            Some(NodeAccess::Storage(StorageNodeAccess {
                trie: self.trie,
                node_index: parent_idx,
            }))
        } else {
            Some(NodeAccess::Branch(BranchNodeAccess {
                trie: self.trie,
                node_index: parent_idx,
            }))
        }
    }

    /// Returns the child of this node given the given index.
    ///
    /// Returns back `self` if there is no such child at this index.
    pub fn child(self, index: Nibble) -> Result<NodeAccess<'a, TUd>, Self> {
        let child_idx = match self.trie.nodes.get(self.node_index).unwrap().children
            [usize::from(u8::from(index))]
        {
            Some(c) => c,
            None => return Err(self),
        };

        if self.trie.nodes.get(child_idx).unwrap().has_storage_value {
            Ok(NodeAccess::Storage(StorageNodeAccess {
                trie: self.trie,
                node_index: child_idx,
            }))
        } else {
            Ok(NodeAccess::Branch(BranchNodeAccess {
                trie: self.trie,
                node_index: child_idx,
            }))
        }
    }

    /// Returns true if this node is the root node of the trie.
    pub fn is_root_node(&self) -> bool {
        self.trie.root_index == Some(self.node_index)
    }

    /// Returns the full key of the node.
    pub fn full_key<'b>(&'b self) -> impl Iterator<Item = Nibble> + 'b {
        self.trie.node_full_key(self.node_index)
    }

    /// Returns the partial key of the node.
    pub fn partial_key<'b>(&'b self) -> impl ExactSizeIterator<Item = Nibble> + 'b {
        self.trie
            .nodes
            .get(self.node_index)
            .unwrap()
            .partial_key
            .iter()
            .cloned()
    }

    /// Adds a storage value to this node, turning it into a [`StorageNodeAccess`].
    ///
    /// The trie structure doesn't change.
    pub fn insert_storage_value(self) -> StorageNodeAccess<'a, TUd> {
        self.trie
            .nodes
            .get_mut(self.node_index)
            .unwrap()
            .has_storage_value = true;

        StorageNodeAccess {
            trie: self.trie,
            node_index: self.node_index,
        }
    }

    /// Returns the user data associated to this node.
    pub fn user_data(&mut self) -> &mut TUd {
        &mut self.trie.nodes.get_mut(self.node_index).unwrap().user_data
    }
}

/// Access to a non-existing node within the [`TrieStructure`].
pub struct Vacant<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,
    key: &'a [Nibble],
    /// Known closest ancestor that is in `trie`. Will become the parent of any newly-inserted
    /// node.
    closest_ancestor: Option<usize>,
}

impl<'a, TUd> Vacant<'a, TUd> {
    /// Prepare the operation of creating the node in question.
    ///
    /// This method analyzes the trie to prepare for the operation, but doesn't actually perform
    /// any insertion. To perform the insertion, use the returned [`PrepareInsert`].
    pub fn insert_storage_value(self) -> PrepareInsert<'a, TUd> {
        // Retrieve what will be the parent after we insert the new node, not taking branching
        // into account yet.
        // If `Some`, contains its index and number of nibbles in its key.
        let future_parent = match (self.closest_ancestor, self.trie.root_index) {
            (Some(_), None) => unreachable!(),
            (Some(ancestor), Some(_)) => {
                let key_len = self.trie.node_full_key(ancestor).count();
                debug_assert!(self.key.len() > key_len);
                Some((ancestor, key_len))
            }
            (None, Some(_)) => None,
            (None, None) => {
                // Situation where the trie is empty. This is kind of a special case that we
                // handle by returning early.
                return PrepareInsert::One(PrepareInsertOne {
                    trie: self.trie,
                    parent: None,
                    partial_key: self.key.to_owned(),
                    children: [None; 16],
                });
            }
        };

        // Get the existing child of `future_parent` that points towards the newly-inserted node,
        // or a successful early-return if none.
        let existing_node_index =
            if let Some((future_parent_index, future_parent_key_len)) = future_parent {
                let new_child_index = self.key[future_parent_key_len];
                let future_parent = self.trie.nodes.get(future_parent_index).unwrap();
                match future_parent.children[usize::from(u8::from(new_child_index))] {
                    Some(i) => {
                        debug_assert_eq!(
                            self.trie.nodes.get(i).unwrap().parent.unwrap().0,
                            future_parent_index
                        );
                        i
                    }
                    None => {
                        // There is an empty slot in `future_parent` for our new node.
                        //
                        //
                        //           `future_parent`
                        //                 +-+
                        //             +-> +-+  <---------+
                        //             |        <----+    |
                        //             |     ^       |    |
                        //            +-+    |       |    |
                        //   New node +-+    +-+-+  +-+  +-+  0 or more existing children
                        //                     +-+  +-+  +-+
                        //
                        //
                        return PrepareInsert::One(PrepareInsertOne {
                            trie: self.trie,
                            parent: Some((future_parent_index, new_child_index)),
                            partial_key: self.key[future_parent_key_len + 1..].to_owned(),
                            children: [None; 16],
                        });
                    }
                }
            } else {
                self.trie.root_index.unwrap()
            };

        // `existing_node_idx` and the new node are known to either have the same parent and the
        // same child index, or to both have no parent. Now let's compare their partial key.
        let existing_node_partial_key = &self
            .trie
            .nodes
            .get(existing_node_index)
            .unwrap()
            .partial_key;
        let new_node_partial_key = &self.key[future_parent.map_or(0, |(_, n)| n + 1)..];
        debug_assert_ne!(&**existing_node_partial_key, new_node_partial_key);
        debug_assert!(!new_node_partial_key.starts_with(existing_node_partial_key));

        // If `new_node_partial_key` starts with `existing_node_partial_key`, then the new node
        // will be inserted in-between the parent and the existing node.
        if new_node_partial_key.starts_with(existing_node_partial_key) {
            // The new node is to be inserted in-between `future_parent` and
            // `existing_node_index`.
            //
            // If `future_parent` is `Some`:
            //
            //
            //                         +-+
            //        `future_parent`  +-+ <---------+
            //                          ^            |
            //                          |            +
            //                         +-+         (0 or more existing children)
            //               New node  +-+
            //                          ^
            //                          |
            //                         +-+
            //  `existing_node_index`  +-+
            //                          ^
            //                          |
            //                          +
            //                    (0 or more existing children)
            //
            //
            //
            // If `future_parent` is `None`:
            //
            //
            //            New node    +-+
            //    (becomes the root)  +-+
            //                         ^
            //                         |
            // `existing_node_index`  +-+
            //     (current root)     +-+
            //                         ^
            //                         |
            //                         +
            //                   (0 or more existing children)
            //

            let mut new_node_children = [None; 16];
            let existing_node_new_child_index =
                existing_node_partial_key[new_node_partial_key.len()];
            new_node_children[usize::from(u8::from(existing_node_new_child_index))] =
                Some(existing_node_index);

            return PrepareInsert::One(PrepareInsertOne {
                trie: self.trie,
                parent: if let Some((future_parent_index, future_parent_key_len)) = future_parent {
                    let new_child_index = self.key[future_parent_key_len];
                    Some((future_parent_index, new_child_index))
                } else {
                    None
                },
                partial_key: new_node_partial_key.to_owned(),
                children: new_node_children,
            });
        }

        // If we reach here, we know that we will need to create a new branch node in addition to
        // the new storage node.
        //
        // If `future_parent` is `Some`:
        //
        //
        //                  `future_parent`
        //
        //                        +-+
        //                        +-+ <--------+  (0 or more existing children)
        //                         ^
        //                         |
        //       New branch node  +-+
        //                        +-+ <-------+
        //                         ^          |
        //                         |          |
        //                        +-+        +-+
        // `existing_node_index`  +-+        +-+  New storage node
        //                         ^
        //                         |
        //
        //                 (0 or more existing children)
        //
        //
        //
        // If `future_parent` is `None`:
        //
        //
        //     New branch node    +-+
        //     (becomes root)     +-+ <-------+
        //                         ^          |
        //                         |          |
        // `existing_node_index`  +-+        +-+
        //     (current root)     +-+        +-+  New storage node
        //                         ^
        //                         |
        //
        //                 (0 or more existing children)
        //
        //

        // Find the common ancestor between `new_node_partial_key` and `existing_node_partial_key`.
        let branch_partial_key_len = {
            debug_assert_ne!(new_node_partial_key, &**existing_node_partial_key);
            let mut len = 0;
            let mut k1 = new_node_partial_key.iter();
            let mut k2 = existing_node_partial_key.iter();
            // Since `new_node_partial_key` is different from `existing_node_partial_key`, we know
            // that `k1.next()` and `k2.next()` won't both be `None`.
            while k1.next() == k2.next() {
                len += 1;
            }
            debug_assert!(len < new_node_partial_key.len());
            debug_assert!(len < existing_node_partial_key.len());
            len
        };

        // Table of children for the new branch node, not including the new storage node.
        // It therefore contains only one entry: `existing_node_index`.
        let branch_children = {
            let mut branch_children = [None; 16];
            let existing_node_new_child_index = existing_node_partial_key[branch_partial_key_len];
            debug_assert_ne!(
                existing_node_new_child_index,
                new_node_partial_key[branch_partial_key_len]
            );
            branch_children[usize::from(u8::from(existing_node_new_child_index))] =
                Some(existing_node_index);
            branch_children
        };

        // Success!
        PrepareInsert::Two(PrepareInsertTwo {
            trie: self.trie,

            storage_child_index: new_node_partial_key[branch_partial_key_len],
            storage_partial_key: new_node_partial_key[branch_partial_key_len + 1..].to_owned(),

            branch_parent: if let Some((future_parent_index, future_parent_key_len)) = future_parent
            {
                let new_child_index = self.key[future_parent_key_len];
                Some((future_parent_index, new_child_index))
            } else {
                None
            },
            branch_partial_key: new_node_partial_key[..branch_partial_key_len].to_owned(),
            branch_children,
        })
    }
}

/// Preparation for a new node insertion.
///
/// The trie hasn't been modified yet and you can safely drop this object.
#[must_use]
pub enum PrepareInsert<'a, TUd> {
    /// One node will be inserted in the trie.
    One(PrepareInsertOne<'a, TUd>),
    /// Two nodes will be inserted in the trie.
    Two(PrepareInsertTwo<'a, TUd>),
}

impl<'a, TUd> PrepareInsert<'a, TUd> {
    /// Insert the new node. `branch_node_user_data` is discarded if `self` is
    /// a [`PrepareInsert::One`].
    pub fn insert(
        self,
        storage_node_user_data: TUd,
        branch_node_user_data: TUd,
    ) -> StorageNodeAccess<'a, TUd> {
        match self {
            PrepareInsert::One(n) => n.insert(storage_node_user_data),
            PrepareInsert::Two(n) => n.insert(storage_node_user_data, branch_node_user_data),
        }
    }
}

/// One node will be inserted in the trie.
pub struct PrepareInsertOne<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,

    /// Value of [`Node::parent`] for the newly-created node.
    /// If `None`, we also set the root of the trie to the new node.
    parent: Option<(usize, Nibble)>,
    /// Value of [`Node::partial_key`] for the newly-created node.
    partial_key: Vec<Nibble>,
    /// Value of [`Node::children`] for the newly-created node.
    children: [Option<usize>; 16],
}

impl<'a, TUd> PrepareInsertOne<'a, TUd> {
    /// Insert the new node.
    pub fn insert(self, user_data: TUd) -> StorageNodeAccess<'a, TUd> {
        let new_node_partial_key_len = self.partial_key.len();

        let new_node_index = self.trie.nodes.insert(Node {
            parent: self.parent,
            partial_key: self.partial_key,
            children: self.children.clone(),
            has_storage_value: true,
            user_data,
        });

        // Update the children node to point to their new parent.
        for (child_index, child) in self.children.iter().enumerate() {
            let mut child = match child {
                Some(c) => self.trie.nodes.get_mut(*c).unwrap(),
                None => continue,
            };

            let child_index = Nibble::try_from(u8::try_from(child_index).unwrap()).unwrap();
            child.parent = Some((new_node_index, child_index));
            child.partial_key = child.partial_key[new_node_partial_key_len + 1..].to_vec();
        }

        // Update the parent to point to its new child.
        if let Some((parent_index, child_index)) = self.parent {
            let mut parent = self.trie.nodes.get_mut(parent_index).unwrap();
            parent.children[usize::from(u8::from(child_index))] = Some(new_node_index);
        } else {
            self.trie.root_index = Some(new_node_index);
        }

        // Success!
        StorageNodeAccess {
            trie: self.trie,
            node_index: new_node_index,
        }
    }
}

/// Two nodes will be inserted in the trie.
pub struct PrepareInsertTwo<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,

    /// Value of the child index in [`Node::parent`] for the newly-created storage node.
    storage_child_index: Nibble,
    /// Value of [`Node::partial_key`] for the newly-created storage node.
    storage_partial_key: Vec<Nibble>,

    /// Value of [`Node::parent`] for the newly-created branch node.
    /// If `None`, we also set the root of the trie to the new branch node.
    branch_parent: Option<(usize, Nibble)>,
    /// Value of [`Node::partial_key`] for the newly-created branch node.
    branch_partial_key: Vec<Nibble>,
    /// Value of [`Node::children`] for the newly-created branch node. Does not include the entry
    /// that must be filled with the new storage node.
    branch_children: [Option<usize>; 16],
}

impl<'a, TUd> PrepareInsertTwo<'a, TUd> {
    /// Key of the branch node that will be inserted.
    pub fn branch_node_key<'b>(&'b self) -> impl Iterator<Item = Nibble> + 'b {
        if let Some((parent_index, child_index)) = self.branch_parent {
            let parent = self.trie.node_full_key(parent_index);
            let iter = parent.chain(iter::once(child_index)).chain(self.branch_partial_key.iter().cloned());
            Either::Left(iter)
        } else {
            Either::Right(self.branch_partial_key.iter().cloned())
        }
    }

    /// Insert the new node.
    pub fn insert(
        self,
        storage_node_user_data: TUd,
        branch_node_user_data: TUd,
    ) -> StorageNodeAccess<'a, TUd> {
        let new_storage_node_partial_key_len = self.storage_partial_key.len();
        let new_branch_node_partial_key_len = self.branch_partial_key.len();

        debug_assert_eq!(
            self.branch_children.iter().filter(|c| c.is_some()).count(),
            1
        );

        let new_branch_node_index = self.trie.nodes.insert(Node {
            parent: self.branch_parent,
            partial_key: self.branch_partial_key,
            children: self.branch_children.clone(),
            has_storage_value: false,
            user_data: branch_node_user_data,
        });

        let new_storage_node_index = self.trie.nodes.insert(Node {
            parent: Some((new_branch_node_index, self.storage_child_index)),
            partial_key: self.storage_partial_key,
            children: [None; 16],
            has_storage_value: true,
            user_data: storage_node_user_data,
        });

        self.trie
            .nodes
            .get_mut(new_branch_node_index)
            .unwrap()
            .children[usize::from(u8::from(self.storage_child_index))] =
            Some(new_storage_node_index);

        // Update the branch node's children to point to their new parent.
        for (child_index, child) in self.branch_children.iter().enumerate() {
            let mut child = match child {
                Some(c) => self.trie.nodes.get_mut(*c).unwrap(),
                None => continue,
            };

            let child_index = Nibble::try_from(u8::try_from(child_index).unwrap()).unwrap();
            child.parent = Some((new_branch_node_index, child_index));
            child.partial_key = child.partial_key[new_branch_node_partial_key_len + 1..].to_vec();
        }

        // Update the branch node's parent to point to its new child.
        if let Some((parent_index, child_index)) = self.branch_parent {
            let mut parent = self.trie.nodes.get_mut(parent_index).unwrap();
            parent.children[usize::from(u8::from(child_index))] = Some(new_branch_node_index);
        } else {
            self.trie.root_index = Some(new_branch_node_index);
        }

        // Success!
        StorageNodeAccess {
            trie: self.trie,
            node_index: new_storage_node_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Nibble, TrieStructure};
    use core::convert::TryFrom as _;

    fn bytes_to_nibbles(bytes: &[u8]) -> Vec<Nibble> {
        bytes
            .iter()
            .map(|b| Nibble::try_from(*b).unwrap())
            .collect()
    }

    // TODO: fuzzing test

    #[test]
    fn basic() {
        let mut trie = TrieStructure::new();
        trie.node(&bytes_to_nibbles(&[1, 2, 3]))
            .into_vacant()
            .unwrap()
            .insert_storage_value()
            .insert((), ());
    }
}
