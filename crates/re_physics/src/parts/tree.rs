//! A dynamic bounding-volume tree over axis-aligned boxes, for the part
//! queries.
//!
//! Leaves hold "fat" boxes (the item's box grown by a margin), so an item
//! that moves a little keeps its leaf. Insertion picks the sibling with the
//! cheapest surface-area increase and rebalances with tree rotations, as in
//! Box2D's dynamic tree, so the tree stays shallow under incremental change.

use glam::Vec3;

/// An axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    /// The minimum corner.
    pub min: Vec3,
    /// The maximum corner.
    pub max: Vec3,
}

impl Aabb {
    /// The box between two corners.
    pub fn new(min: Vec3, max: Vec3) -> Aabb {
        Aabb { min, max }
    }

    fn union(&self, o: &Aabb) -> Aabb {
        Aabb {
            min: self.min.min(o.min),
            max: self.max.max(o.max),
        }
    }

    fn area(&self) -> f32 {
        let d = (self.max - self.min).max(Vec3::ZERO);
        2.0 * (d.x * d.y + d.y * d.z + d.z * d.x)
    }

    fn contains(&self, o: &Aabb) -> bool {
        self.min.cmple(o.min).all() && self.max.cmpge(o.max).all()
    }

    /// True when the boxes overlap (touching counts).
    pub fn overlaps(&self, o: &Aabb) -> bool {
        self.min.cmple(o.max).all() && o.min.cmple(self.max).all()
    }

    fn grown(&self, m: f32) -> Aabb {
        Aabb {
            min: self.min - Vec3::splat(m),
            max: self.max + Vec3::splat(m),
        }
    }

    /// The entry distance of a ray `origin + t * dir` (`inv` = 1 / dir) into the
    /// box, if it enters within `[0, max_t]`.
    fn ray_enter(&self, origin: Vec3, inv: Vec3, max_t: f32) -> Option<f32> {
        let t1 = (self.min - origin) * inv;
        let t2 = (self.max - origin) * inv;
        let tmin = t1.min(t2);
        let tmax = t1.max(t2);
        // NaN (0 * inf) components come from a ray on a slab boundary; treat them as
        // unbounded.
        let lo = [tmin.x, tmin.y, tmin.z]
            .into_iter()
            .filter(|v| !v.is_nan())
            .fold(0.0f32, f32::max);
        let hi = [tmax.x, tmax.y, tmax.z]
            .into_iter()
            .filter(|v| !v.is_nan())
            .fold(max_t, f32::min);
        (lo <= hi).then_some(lo)
    }
}

const NONE: u32 = u32::MAX;

#[derive(Clone, Debug)]
struct Node<T> {
    aabb: Aabb,
    parent: u32,
    left: u32,
    right: u32,
    height: i32,
    item: Option<T>,
}

impl<T> Node<T> {
    fn is_leaf(&self) -> bool {
        self.left == NONE
    }
}

/// The tree. `T` is the item stored in each leaf.
#[derive(Clone, Debug)]
pub struct Tree<T> {
    nodes: Vec<Node<T>>,
    free: Vec<u32>,
    root: u32,
    margin: f32,
    leaves: usize,
}

impl<T: Copy> Tree<T> {
    /// An empty tree whose leaves are grown by `margin`.
    pub fn new(margin: f32) -> Tree<T> {
        Tree {
            nodes: Vec::new(),
            free: Vec::new(),
            root: NONE,
            margin,
            leaves: 0,
        }
    }

    /// The number of items.
    pub fn len(&self) -> usize {
        self.leaves
    }

    /// True when the tree holds nothing.
    pub fn is_empty(&self) -> bool {
        self.leaves == 0
    }

    fn alloc(&mut self, node: Node<T>) -> u32 {
        if let Some(i) = self.free.pop() {
            self.nodes[i as usize] = node;
            i
        } else {
            self.nodes.push(node);
            (self.nodes.len() - 1) as u32
        }
    }

    fn release(&mut self, i: u32) {
        let n = &mut self.nodes[i as usize];
        n.item = None;
        n.height = -1;
        n.left = NONE;
        n.right = NONE;
        n.parent = NONE;
        self.free.push(i);
    }

    /// Adds an item with box `aabb`; returns its proxy id.
    pub fn insert(&mut self, aabb: Aabb, item: T) -> u32 {
        let leaf = self.alloc(Node {
            aabb: aabb.grown(self.margin),
            parent: NONE,
            left: NONE,
            right: NONE,
            height: 0,
            item: Some(item),
        });
        self.insert_leaf(leaf);
        self.leaves += 1;
        leaf
    }

    /// Removes a proxy.
    pub fn remove(&mut self, proxy: u32) {
        self.remove_leaf(proxy);
        self.release(proxy);
        self.leaves -= 1;
    }

    /// Updates a proxy's box. Returns true when the leaf had to move (the new box
    /// left its fat box).
    pub fn update(&mut self, proxy: u32, aabb: Aabb) -> bool {
        if self.nodes[proxy as usize].aabb.contains(&aabb) {
            return false;
        }
        self.remove_leaf(proxy);
        self.nodes[proxy as usize].aabb = aabb.grown(self.margin);
        self.insert_leaf(proxy);
        true
    }

    /// The item of a proxy.
    pub fn item(&self, proxy: u32) -> Option<T> {
        self.nodes.get(proxy as usize).and_then(|n| n.item)
    }

    fn insert_leaf(&mut self, leaf: u32) {
        if self.root == NONE {
            self.root = leaf;
            self.nodes[leaf as usize].parent = NONE;
            return;
        }
        let leaf_box = self.nodes[leaf as usize].aabb;
        // Find the best sibling by the surface-area heuristic.
        let mut index = self.root;
        while !self.nodes[index as usize].is_leaf() {
            let n = &self.nodes[index as usize];
            let (left, right) = (n.left, n.right);
            let area = n.aabb.area();
            let combined = n.aabb.union(&leaf_box).area();
            let cost = 2.0 * combined;
            let inherit = 2.0 * (combined - area);
            let child_cost = |c: u32| {
                let cn = &self.nodes[c as usize];
                let u = cn.aabb.union(&leaf_box).area();
                if cn.is_leaf() {
                    u + inherit
                } else {
                    u - cn.aabb.area() + inherit
                }
            };
            let (c1, c2) = (child_cost(left), child_cost(right));
            if cost < c1 && cost < c2 {
                break;
            }
            index = if c1 < c2 { left } else { right };
        }
        let sibling = index;
        let old_parent = self.nodes[sibling as usize].parent;
        let new_parent = self.alloc(Node {
            aabb: leaf_box.union(&self.nodes[sibling as usize].aabb),
            parent: old_parent,
            left: sibling,
            right: leaf,
            height: self.nodes[sibling as usize].height + 1,
            item: None,
        });
        if old_parent == NONE {
            self.root = new_parent;
        } else {
            let p = &mut self.nodes[old_parent as usize];
            if p.left == sibling {
                p.left = new_parent;
            } else {
                p.right = new_parent;
            }
        }
        self.nodes[sibling as usize].parent = new_parent;
        self.nodes[leaf as usize].parent = new_parent;
        self.refit(new_parent);
    }

    fn remove_leaf(&mut self, leaf: u32) {
        if leaf == self.root {
            self.root = NONE;
            return;
        }
        let parent = self.nodes[leaf as usize].parent;
        let grand = self.nodes[parent as usize].parent;
        let sibling = if self.nodes[parent as usize].left == leaf {
            self.nodes[parent as usize].right
        } else {
            self.nodes[parent as usize].left
        };
        if grand == NONE {
            self.root = sibling;
            self.nodes[sibling as usize].parent = NONE;
            self.release(parent);
        } else {
            let g = &mut self.nodes[grand as usize];
            if g.left == parent {
                g.left = sibling;
            } else {
                g.right = sibling;
            }
            self.nodes[sibling as usize].parent = grand;
            self.release(parent);
            self.refit(grand);
        }
        self.nodes[leaf as usize].parent = NONE;
    }

    /// Walks up from `i`, rebalancing and refitting boxes and heights.
    fn refit(&mut self, mut i: u32) {
        while i != NONE {
            i = self.balance(i);
            let (l, r) = (self.nodes[i as usize].left, self.nodes[i as usize].right);
            let hl = self.nodes[l as usize].height;
            let hr = self.nodes[r as usize].height;
            let b = self.nodes[l as usize]
                .aabb
                .union(&self.nodes[r as usize].aabb);
            let n = &mut self.nodes[i as usize];
            n.height = 1 + hl.max(hr);
            n.aabb = b;
            i = n.parent;
        }
    }

    /// A tree rotation at `a` when its children's heights differ by more than one;
    /// returns the node now at `a`'s place.
    fn balance(&mut self, a: u32) -> u32 {
        let na = &self.nodes[a as usize];
        if na.is_leaf() || na.height < 2 {
            return a;
        }
        let (b, c) = (na.left, na.right);
        let balance = self.nodes[c as usize].height - self.nodes[b as usize].height;
        if balance > 1 {
            return self.rotate(a, c, b, true);
        }
        if balance < -1 {
            return self.rotate(a, b, c, false);
        }
        a
    }

    /// Promotes `up` (a child of `a`) over `a`; `other` is `a`'s other child.
    /// `up_is_right` says which side `up` was on.
    fn rotate(&mut self, a: u32, up: u32, _other: u32, up_is_right: bool) -> u32 {
        let (f, g) = (self.nodes[up as usize].left, self.nodes[up as usize].right);
        // Swap a and up.
        let a_parent = self.nodes[a as usize].parent;
        self.nodes[up as usize].left = a;
        self.nodes[up as usize].parent = a_parent;
        self.nodes[a as usize].parent = up;
        if a_parent == NONE {
            self.root = up;
        } else {
            let p = &mut self.nodes[a_parent as usize];
            if p.left == a {
                p.left = up;
            } else {
                p.right = up;
            }
        }
        // Keep the taller grandchild under `up`; give the other to `a`.
        let (keep, give) = if self.nodes[f as usize].height > self.nodes[g as usize].height {
            (f, g)
        } else {
            (g, f)
        };
        self.nodes[up as usize].right = keep;
        if up_is_right {
            self.nodes[a as usize].right = give;
        } else {
            self.nodes[a as usize].left = give;
        }
        self.nodes[give as usize].parent = a;
        // Refit a, then up.
        for n in [a, up] {
            let (l, r) = (self.nodes[n as usize].left, self.nodes[n as usize].right);
            let b = self.nodes[l as usize]
                .aabb
                .union(&self.nodes[r as usize].aabb);
            let h = 1 + self.nodes[l as usize]
                .height
                .max(self.nodes[r as usize].height);
            let node = &mut self.nodes[n as usize];
            node.aabb = b;
            node.height = h;
        }
        up
    }

    /// Calls `f` for every item whose fat box overlaps `aabb`.
    pub fn query(&self, aabb: &Aabb, mut f: impl FnMut(T)) {
        if self.root == NONE {
            return;
        }
        let mut stack = vec![self.root];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i as usize];
            if !n.aabb.overlaps(aabb) {
                continue;
            }
            if let Some(item) = n.item {
                f(item);
            } else {
                stack.push(n.left);
                stack.push(n.right);
            }
        }
    }

    /// Walks the items whose fat boxes a ray `origin + t * dir` enters within
    /// `[0, max_t]`, nearest box first. `f` returns the new `max_t` (the distance
    /// of the closest hit so far), which prunes the rest.
    pub fn ray(&self, origin: Vec3, dir: Vec3, max_t: f32, mut f: impl FnMut(T, f32) -> f32) {
        if self.root == NONE {
            return;
        }
        let inv = Vec3::ONE / dir;
        let mut max_t = max_t;
        // A small best-first walk: entries sorted by entry distance.
        let mut stack: Vec<(f32, u32)> = Vec::new();
        if let Some(t) = self.nodes[self.root as usize]
            .aabb
            .ray_enter(origin, inv, max_t)
        {
            stack.push((t, self.root));
        }
        while let Some((t, i)) = stack.pop() {
            if t > max_t {
                continue;
            }
            let n = &self.nodes[i as usize];
            if let Some(item) = n.item {
                max_t = f(item, max_t).min(max_t);
                continue;
            }
            let mut kids = [(f32::INFINITY, NONE); 2];
            for (k, c) in [n.left, n.right].into_iter().enumerate() {
                if let Some(tc) = self.nodes[c as usize].aabb.ray_enter(origin, inv, max_t) {
                    kids[k] = (tc, c);
                }
            }
            // Push the farther child first so the nearer one is visited first.
            kids.sort_by(|a, b| b.0.total_cmp(&a.0));
            for (tc, c) in kids {
                if c != NONE {
                    stack.push((tc, c));
                }
            }
        }
    }

    /// The tree's height (0 for a single leaf, -1 when empty).
    pub fn height(&self) -> i32 {
        if self.root == NONE {
            -1
        } else {
            self.nodes[self.root as usize].height
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 11) as f32 / (1u64 << 53) as f32
    }

    #[test]
    fn queries_match_brute_force_under_churn() {
        let mut seed = 99u64;
        let mut tree: Tree<u32> = Tree::new(0.1);
        let mut boxes: Vec<Option<(u32, Aabb)>> = Vec::new();
        for i in 0..2000u32 {
            let c = Vec3::new(rng(&mut seed), rng(&mut seed), rng(&mut seed)) * 100.0;
            let h = Vec3::new(rng(&mut seed), rng(&mut seed), rng(&mut seed)) * 3.0 + 0.1;
            let b = Aabb::new(c - h, c + h);
            boxes.push(Some((tree.insert(b, i), b)));
        }
        for step in 0..3000u32 {
            let i = (rng(&mut seed) * boxes.len() as f32) as usize % boxes.len();
            match boxes[i] {
                Some((p, b)) if step % 3 == 0 => {
                    tree.remove(p);
                    boxes[i] = None;
                    let _ = b;
                }
                Some((p, _)) => {
                    let c = Vec3::new(rng(&mut seed), rng(&mut seed), rng(&mut seed)) * 100.0;
                    let b = Aabb::new(c - 1.0, c + 1.0);
                    tree.update(p, b);
                    boxes[i] = Some((p, b));
                }
                None => {
                    let c = Vec3::new(rng(&mut seed), rng(&mut seed), rng(&mut seed)) * 100.0;
                    let b = Aabb::new(c - 2.0, c + 2.0);
                    boxes[i] = Some((tree.insert(b, i as u32), b));
                }
            }
        }
        assert!(tree.height() < 40, "height {}", tree.height());
        for _ in 0..200 {
            let c = Vec3::new(rng(&mut seed), rng(&mut seed), rng(&mut seed)) * 100.0;
            let q = Aabb::new(c - 5.0, c + 5.0);
            let mut got = Vec::new();
            tree.query(&q, |i| got.push(i));
            got.sort();
            // The tree reports fat boxes: every true overlap must be among them.
            for (i, b) in boxes.iter().enumerate() {
                if let Some((p, b)) = b
                    && b.overlaps(&q)
                {
                    assert_eq!(tree.item(*p), Some(i as u32));
                    assert!(got.binary_search(&(i as u32)).is_ok());
                }
            }
        }
        let live = boxes.iter().filter(|b| b.is_some()).count();
        assert_eq!(tree.len(), live);
    }

    #[test]
    fn ray_visits_nearest_first() {
        let mut tree: Tree<u32> = Tree::new(0.0);
        for i in 0..10u32 {
            let x = i as f32 * 10.0;
            tree.insert(
                Aabb::new(Vec3::new(x, -1.0, -1.0), Vec3::new(x + 1.0, 1.0, 1.0)),
                i,
            );
        }
        let mut seen = Vec::new();
        tree.ray(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 1000.0, |i, max| {
            seen.push(i);
            if i == 3 { 38.0 } else { max }
        });
        assert_eq!(seen.first(), Some(&0));
        assert!(seen.contains(&3));
        assert!(!seen.contains(&5), "{seen:?}");
    }
}
