//! A link-cut tree (Sleator and Tarjan, "A data structure for dynamic
//! trees", 1983) over the document's nodes, with template contents hanging
//! under their templates. It answers "is `a` a host-including inclusive
//! ancestor of `b`" in O(log n) amortised time, whatever the depth, while
//! the tree changes by links and cuts.
//!
//! Every node is in one auxiliary splay tree, keyed by depth, holding one
//! path of the real tree (a "preferred path"). An auxiliary tree's root
//! keeps a "path-parent" pointer to the real parent of the path's top node,
//! in the same `parent` field: a node is an auxiliary root when its
//! `parent` is none or does not have it as a child.
//!
//! Linking a fresh node (the parser's common case) is O(1): it is alone in
//! its auxiliary tree, so linking only sets its path-parent pointer.

const NONE: u32 = u32::MAX;

#[derive(Clone, Debug, Default)]
pub(crate) struct LinkCut {
    left: Vec<u32>,
    right: Vec<u32>,
    parent: Vec<u32>,
    /// Rotations done, so tests can check the amortised bound.
    pub(crate) rotations: u64,
}

impl LinkCut {
    /// Add a node with no parent.
    pub(crate) fn push(&mut self) {
        self.left.push(NONE);
        self.right.push(NONE);
        self.parent.push(NONE);
    }

    fn is_aux_root(&self, x: u32) -> bool {
        let p = self.parent[x as usize];
        p == NONE || (self.left[p as usize] != x && self.right[p as usize] != x)
    }

    /// Rotate `x` above its auxiliary parent.
    fn rotate(&mut self, x: u32) {
        self.rotations += 1;
        let p = self.parent[x as usize];
        let g = self.parent[p as usize];
        let p_was_root = self.is_aux_root(p);
        if self.left[p as usize] == x {
            let b = self.right[x as usize];
            self.left[p as usize] = b;
            if b != NONE {
                self.parent[b as usize] = p;
            }
            self.right[x as usize] = p;
        } else {
            let b = self.left[x as usize];
            self.right[p as usize] = b;
            if b != NONE {
                self.parent[b as usize] = p;
            }
            self.left[x as usize] = p;
        }
        self.parent[p as usize] = x;
        // `x` takes `p`'s place: as `g`'s child, or carrying `p`'s
        // path-parent pointer.
        self.parent[x as usize] = g;
        if !p_was_root {
            if self.left[g as usize] == p {
                self.left[g as usize] = x;
            } else {
                self.right[g as usize] = x;
            }
        }
    }

    /// Make `x` the root of its auxiliary tree.
    fn splay(&mut self, x: u32) {
        while !self.is_aux_root(x) {
            let p = self.parent[x as usize];
            if !self.is_aux_root(p) {
                let g = self.parent[p as usize];
                let zigzig = (self.left[g as usize] == p) == (self.left[p as usize] == x);
                if zigzig {
                    self.rotate(p);
                } else {
                    self.rotate(x);
                }
            }
            self.rotate(x);
        }
    }

    /// Make the path from the root to `v` preferred, ending at `v`, with `v`
    /// the root of its auxiliary tree. Returns the last node where the walk
    /// joined a path: after `access(u)`, `access(v)` returns the lowest
    /// common ancestor of `u` and `v` (if they are in one tree).
    fn access(&mut self, v: u32) -> u32 {
        let mut last = NONE;
        let mut x = v;
        while x != NONE {
            self.splay(x);
            self.right[x as usize] = last;
            last = x;
            x = self.parent[x as usize];
        }
        self.splay(v);
        last
    }

    /// Make `child`, a tree root, a child of `parent` (in another tree).
    pub(crate) fn link(&mut self, child: u32, parent: u32) {
        // `child` is the shallowest node of its auxiliary tree, so as that
        // tree's root it has nothing on its left, and the tree's
        // path-parent pointer (none, for a tree root) becomes `parent`. A
        // fresh node is already its auxiliary tree's root.
        self.splay(child);
        debug_assert_eq!(self.left[child as usize], NONE, "linked node is not a root");
        debug_assert_eq!(
            self.parent[child as usize], NONE,
            "linked node is not a root"
        );
        self.parent[child as usize] = parent;
    }

    /// Detach `child` from its parent.
    pub(crate) fn cut(&mut self, child: u32) {
        self.access(child);
        let l = self.left[child as usize];
        debug_assert_ne!(l, NONE, "cut node has no parent");
        self.parent[l as usize] = NONE;
        self.left[child as usize] = NONE;
    }

    /// The parent of `v` in the represented tree, for checks.
    pub(crate) fn tree_parent(&mut self, v: u32) -> Option<u32> {
        self.access(v);
        let mut x = self.left[v as usize];
        if x == NONE {
            return None;
        }
        while self.right[x as usize] != NONE {
            x = self.right[x as usize];
        }
        self.splay(x);
        Some(x)
    }

    /// Is `a` an inclusive ancestor of `b`?
    pub(crate) fn is_inclusive_ancestor(&mut self, a: u32, b: u32) -> bool {
        if a == b {
            return true;
        }
        // `access(b)` returns the lowest common ancestor when `a` and `b`
        // share a tree, and otherwise a node of `b`'s tree, never `a`.
        self.access(a);
        self.access(b) == a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The same forest with plain parent pointers.
    struct Naive(Vec<Option<u32>>);

    impl Naive {
        fn is_inclusive_ancestor(&self, a: u32, b: u32) -> bool {
            let mut x = Some(b);
            while let Some(n) = x {
                if n == a {
                    return true;
                }
                x = self.0[n as usize];
            }
            false
        }
    }

    #[derive(Clone, Debug)]
    enum Op {
        Link(u32, u32),
        Cut(u32),
        Query(u32, u32),
    }

    fn op(n: u32) -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0..n, 0..n).prop_map(|(a, b)| Op::Link(a, b)),
            2 => (0..n).prop_map(Op::Cut),
            4 => (0..n, 0..n).prop_map(|(a, b)| Op::Query(a, b)),
        ]
    }

    fn cases() -> u32 {
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(cases()))]

        /// Random links (of roots, without cycles), cuts and queries agree
        /// with the plain forest, and every query of every pair agrees at
        /// the end.
        #[test]
        fn matches_a_plain_forest(n in 1u32..40, ops in prop::collection::vec(op(40), 0..300)) {
            let mut lc = LinkCut::default();
            let mut naive = Naive(vec![None; n as usize]);
            for _ in 0..n {
                lc.push();
            }
            for op in ops {
                match op {
                    Op::Link(c, p) => {
                        let (c, p) = (c % n, p % n);
                        if naive.0[c as usize].is_none() && !naive.is_inclusive_ancestor(c, p) {
                            lc.link(c, p);
                            naive.0[c as usize] = Some(p);
                        }
                    }
                    Op::Cut(c) => {
                        let c = c % n;
                        if naive.0[c as usize].is_some() {
                            lc.cut(c);
                            naive.0[c as usize] = None;
                        }
                    }
                    Op::Query(a, b) => {
                        let (a, b) = (a % n, b % n);
                        prop_assert_eq!(
                            lc.is_inclusive_ancestor(a, b),
                            naive.is_inclusive_ancestor(a, b),
                            "{} above {}", a, b
                        );
                    }
                }
            }
            for a in 0..n {
                prop_assert_eq!(lc.tree_parent(a), naive.0[a as usize], "parent of {}", a);
                for b in 0..n {
                    prop_assert_eq!(
                        lc.is_inclusive_ancestor(a, b),
                        naive.is_inclusive_ancestor(a, b),
                        "{} above {}", a, b
                    );
                }
            }
        }
    }

    /// Rotations for queries on a path of `n` nodes in a few orders.
    fn path_rotations(n: u32) -> Vec<u64> {
        let orders: [&dyn Fn(u32) -> (u32, u32); 4] = [
            &|i| (0, i),
            &|i| (i, n - 1),
            &|i| ((i * 7919) % n, (i * 104_729) % n),
            &|i| (n - 1 - i, n - 1),
        ];
        orders
            .iter()
            .map(|f| {
                let mut lc = LinkCut::default();
                for i in 0..n {
                    lc.push();
                    if i > 0 {
                        lc.link(i, i - 1);
                    }
                }
                for i in 0..n {
                    let (a, b) = f(i);
                    lc.is_inclusive_ancestor(a, b);
                }
                lc.rotations
            })
            .collect()
    }

    /// Splaying's amortised bound, O(log n) per operation, measured: a few
    /// rotations times log2 n per query on paths, in orders that defeat
    /// plain move-to-root.
    #[test]
    fn rotations_stay_logarithmic() {
        // Measured: 5, 4, 31 and 7 per query. Rotating a node twice
        // instead of its parent first (move-to-root) gives 8194 per query on
        // the last order, linear in n.
        let n: u32 = 1 << 14;
        let bound = 3 * u64::from(n.ilog2()) * u64::from(n);
        for (order, r) in path_rotations(n).into_iter().enumerate() {
            assert!(
                (u64::from(n)..=bound).contains(&r),
                "order {order}: {r} rotations for {n} queries"
            );
        }
    }

    /// A path of 100,000 nodes, queried from the deep end and the shallow
    /// end over and over: amortised O(log n), so this is quick; a walk per
    /// query would be 10^10 steps.
    #[test]
    fn deep_paths_are_cheap() {
        let n = 100_000u32;
        let mut lc = LinkCut::default();
        for i in 0..n {
            lc.push();
            if i > 0 {
                lc.link(i, i - 1);
            }
        }
        for i in 0..n {
            assert!(lc.is_inclusive_ancestor(i, n - 1));
            assert!(!lc.is_inclusive_ancestor(n - 1, i % (n - 1)));
            assert!(lc.is_inclusive_ancestor(0, i));
        }
        // Cut in the middle and hang the bottom half under the top again.
        lc.cut(n / 2);
        assert!(!lc.is_inclusive_ancestor(0, n - 1));
        lc.link(n / 2, 7);
        assert!(lc.is_inclusive_ancestor(7, n - 1));
        assert!(!lc.is_inclusive_ancestor(8, n - 1));
    }
}
