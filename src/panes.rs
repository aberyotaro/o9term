//! ウィンドウ内のペイン分割を表す二分木。
//! 葉ごとに独立した PTY (Terminal) が動く。

use crate::renderer::Rect;
use crate::term::{PaneId, Terminal};

pub struct Pane<T = Terminal> {
    pub id: PaneId,
    pub terminal: T,
    pub title: String,
}

/// スプリッター(仕切り線)の向き。Vertical = 縦線で左右に分割。
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SplitDir {
    Vertical,
    Horizontal,
}

pub enum PaneNode<T = Terminal> {
    /// ペインがない状態、または木の組み替え中のプレースホルダ。
    Empty,
    Leaf(Pane<T>),
    Split { dir: SplitDir, ratio: f32, first: Box<PaneNode<T>>, second: Box<PaneNode<T>> },
}

/// スプリッターのヒットテスト・描画用情報。
/// `index` は木の行きがけ順の通し番号で、ドラッグ中の比率更新に使う。
pub struct SplitterHit {
    pub index: usize,
    pub dir: SplitDir,
    /// スプリッターが動ける範囲 (分割ノード全体の矩形)
    pub track: Rect,
    pub rect: Rect,
}

impl<T> PaneNode<T> {
    /// 矩形を再帰的に分配し、ペインとスプリッターの配置を計算する。
    pub fn layout(
        &self,
        rect: Rect,
        sw: i32,
        panes: &mut Vec<(PaneId, Rect)>,
        splitters: &mut Vec<SplitterHit>,
    ) {
        match self {
            PaneNode::Empty => {}
            PaneNode::Leaf(pane) => panes.push((pane.id, rect)),
            PaneNode::Split { dir, ratio, first, second } => {
                let index = splitters.len();
                let (r1, sp, r2) = match dir {
                    SplitDir::Vertical => {
                        let fw = (((rect.w - sw).max(0)) as f32 * ratio).round() as i32;
                        (
                            Rect { x: rect.x, y: rect.y, w: fw, h: rect.h },
                            Rect { x: rect.x + fw, y: rect.y, w: sw, h: rect.h },
                            Rect {
                                x: rect.x + fw + sw,
                                y: rect.y,
                                w: (rect.w - fw - sw).max(0),
                                h: rect.h,
                            },
                        )
                    }
                    SplitDir::Horizontal => {
                        let fh = (((rect.h - sw).max(0)) as f32 * ratio).round() as i32;
                        (
                            Rect { x: rect.x, y: rect.y, w: rect.w, h: fh },
                            Rect { x: rect.x, y: rect.y + fh, w: rect.w, h: sw },
                            Rect {
                                x: rect.x,
                                y: rect.y + fh + sw,
                                w: rect.w,
                                h: (rect.h - fh - sw).max(0),
                            },
                        )
                    }
                };
                splitters.push(SplitterHit { index, dir: *dir, track: rect, rect: sp });
                first.layout(r1, sw, panes, splitters);
                second.layout(r2, sw, panes, splitters);
            }
        }
    }

    /// 行きがけ順 `target` 番目の分割ノードの比率を更新する。
    /// 順序は `layout` と一致させてある。
    pub fn set_ratio(&mut self, target: usize, value: f32) {
        let mut counter = 0;
        self.set_ratio_inner(target, value, &mut counter);
    }

    fn set_ratio_inner(&mut self, target: usize, value: f32, counter: &mut usize) -> bool {
        if let PaneNode::Split { ratio, first, second, .. } = self {
            let index = *counter;
            *counter += 1;
            if index == target {
                *ratio = value;
                return true;
            }
            first.set_ratio_inner(target, value, counter)
                || second.set_ratio_inner(target, value, counter)
        } else {
            false
        }
    }

    /// `target` の葉を `dir` で分割し、`subtree` を `before` に応じて
    /// 前側 (左/上) または後側 (右/下) に挿入する。
    pub fn dock_subtree(
        &mut self,
        target: PaneId,
        dir: SplitDir,
        before: bool,
        subtree: &mut Option<PaneNode<T>>,
    ) -> bool {
        match self {
            PaneNode::Leaf(pane) if pane.id == target => {
                let Some(sub) = subtree.take() else { return false };
                let old = std::mem::replace(self, PaneNode::Empty);
                let (first, second) = if before { (sub, old) } else { (old, sub) };
                *self = PaneNode::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            PaneNode::Split { first, second, .. } => {
                first.dock_subtree(target, dir, before, subtree)
                    || second.dock_subtree(target, dir, before, subtree)
            }
            _ => false,
        }
    }

    /// 葉を所有権ごと取り出し、空いた分割を縮約する。PTY は停止しない。
    pub fn take_pane(&mut self, id: PaneId) -> Option<Pane<T>> {
        match self {
            PaneNode::Leaf(pane) if pane.id == id => {
                match std::mem::replace(self, PaneNode::Empty) {
                    PaneNode::Leaf(pane) => Some(pane),
                    _ => unreachable!(),
                }
            }
            PaneNode::Split { first, second, .. } => {
                let pane = first.take_pane(id).or_else(|| second.take_pane(id))?;
                if matches!(**first, PaneNode::Empty) {
                    *self = std::mem::replace(&mut **second, PaneNode::Empty);
                } else if matches!(**second, PaneNode::Empty) {
                    *self = std::mem::replace(&mut **first, PaneNode::Empty);
                }
                Some(pane)
            }
            _ => None,
        }
    }

    pub fn find_pane(&self, id: PaneId) -> Option<&Pane<T>> {
        match self {
            PaneNode::Leaf(pane) if pane.id == id => Some(pane),
            PaneNode::Split { first, second, .. } => {
                first.find_pane(id).or_else(|| second.find_pane(id))
            }
            _ => None,
        }
    }

    pub fn first_pane(&self) -> Option<&Pane<T>> {
        match self {
            PaneNode::Leaf(pane) => Some(pane),
            PaneNode::Split { first, second, .. } => {
                first.first_pane().or_else(|| second.first_pane())
            }
            PaneNode::Empty => None,
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.find_pane(id).is_some()
    }

    /// 全ペインに対して処理を実行する。
    pub fn for_each_pane_mut(&mut self, f: &mut impl FnMut(&mut Pane<T>)) {
        match self {
            PaneNode::Leaf(pane) => f(pane),
            PaneNode::Split { first, second, .. } => {
                first.for_each_pane_mut(f);
                second.for_each_pane_mut(f);
            }
            PaneNode::Empty => {}
        }
    }
}

/// 1 ウィンドウ内のペイン構成とフォーカス。
pub struct Panes<T = Terminal> {
    pub root: PaneNode<T>,
    pub focused: PaneId,
}

impl<T> Default for Panes<T> {
    fn default() -> Self {
        Self { root: PaneNode::Empty, focused: 0 }
    }
}

impl<T> Panes<T> {
    pub fn new(pane: Pane<T>) -> Self {
        Self { focused: pane.id, root: PaneNode::Leaf(pane) }
    }

    /// ウィンドウの表示タイトル = フォーカス中ペインのタイトル (cwd のフルパス)。
    pub fn title(&self) -> &str {
        self.root
            .find_pane(self.focused)
            .or_else(|| self.root.first_pane())
            .map(|p| p.title.as_str())
            .unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.root, PaneNode::Empty)
    }

    /// ペインを閉じ、フォーカスが消えた場合は残った最初のペインに移す。
    pub fn close(&mut self, id: PaneId) -> bool {
        if self.root.take_pane(id).is_none() {
            return false;
        }
        if !self.root.contains(self.focused) {
            self.focused = self.root.first_pane().map(|pane| pane.id).unwrap_or(0);
        }
        true
    }

    /// 同じウィンドウ内で葉の隣へ移す。移動前に両端を検証し、PTY を保持する。
    pub fn move_pane(
        &mut self,
        source: PaneId,
        target: PaneId,
        dir: SplitDir,
        before: bool,
    ) -> bool {
        if source == target || !self.root.contains(source) || !self.root.contains(target) {
            return false;
        }
        let Some(pane) = self.root.take_pane(source) else { return false };
        let mut carrier = Some(PaneNode::Leaf(pane));
        let inserted = self.root.dock_subtree(target, dir, before, &mut carrier);
        // A validated destination cannot disappear when a distinct leaf is removed.
        debug_assert!(inserted);
        self.focused = source;
        inserted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A stand-in owned resource detects accidental terminal shutdown.
    struct Session {
        id: PaneId,
        closed: Rc<RefCell<Vec<PaneId>>>,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            self.closed.borrow_mut().push(self.id);
        }
    }

    fn pane(id: PaneId, closed: &Rc<RefCell<Vec<PaneId>>>) -> Pane<Session> {
        Pane {
            id,
            terminal: Session { id, closed: closed.clone() },
            title: format!("/session/{id}"),
        }
    }

    fn leaf(id: PaneId, closed: &Rc<RefCell<Vec<PaneId>>>) -> PaneNode<Session> {
        PaneNode::Leaf(pane(id, closed))
    }

    fn split(dir: SplitDir, ratio: f32, first: PaneNode<Session>, second: PaneNode<Session>) -> PaneNode<Session> {
        PaneNode::Split { dir, ratio, first: Box::new(first), second: Box::new(second) }
    }

    fn nested(closed: &Rc<RefCell<Vec<PaneId>>>) -> Panes<Session> {
        Panes {
            focused: 2,
            root: split(
                SplitDir::Horizontal,
                0.35,
                split(SplitDir::Vertical, 0.25, leaf(1, closed), leaf(2, closed)),
                split(SplitDir::Vertical, 0.7, leaf(3, closed), leaf(4, closed)),
            ),
        }
    }

    fn ids(node: &PaneNode<Session>) -> Vec<PaneId> {
        match node {
            PaneNode::Leaf(pane) => vec![pane.id],
            PaneNode::Split { first, second, .. } => {
                assert!(!matches!(**first, PaneNode::Empty));
                assert!(!matches!(**second, PaneNode::Empty));
                let mut all = ids(first);
                all.extend(ids(second));
                all
            }
            PaneNode::Empty => Vec::new(),
        }
    }

    fn assert_valid(panes: &Panes<Session>, expected: &[PaneId]) {
        if expected.is_empty() {
            assert!(matches!(panes.root, PaneNode::Empty));
            assert_eq!(panes.focused, 0);
        } else {
            assert!(panes.root.contains(panes.focused), "focus must name a remaining pane");
        }
        let mut all = ids(&panes.root);
        all.sort_unstable();
        assert_eq!(all, expected, "each remaining pane must occur exactly once");
    }

    #[test]
    fn empty_window_has_no_focus_or_title() {
        let panes = Panes::<Session>::default();
        assert!(panes.is_empty());
        assert_eq!(panes.title(), "");
        assert_valid(&panes, &[]);
    }

    #[test]
    fn inserting_in_four_directions_places_new_pane_on_requested_side() {
        for dir in [SplitDir::Vertical, SplitDir::Horizontal] {
            for before in [true, false] {
                let closed = Rc::new(RefCell::new(Vec::new()));
                let mut panes = Panes::new(pane(1, &closed));
                let mut carrier = Some(leaf(2, &closed));
                assert!(panes.root.dock_subtree(1, dir, before, &mut carrier));
                assert!(carrier.is_none());
                assert_valid(&panes, &[1, 2]);
                let mut positions = Vec::new();
                let mut splitters = Vec::new();
                panes.root.layout(Rect { x: 10, y: 20, w: 204, h: 104 }, 4, &mut positions, &mut splitters);
                assert_eq!(splitters.len(), 1);
                assert_eq!(splitters[0].dir, dir);
                assert_eq!(positions[0].0, if before { 2 } else { 1 });
                assert_eq!(positions[1].0, if before { 1 } else { 2 });
                let (first, second) = (positions[0].1, positions[1].1);
                match dir {
                    SplitDir::Vertical => {
                        assert_eq!((first.x, first.y, first.w, first.h), (10, 20, 100, 104));
                        assert_eq!((second.x, second.y, second.w, second.h), (114, 20, 100, 104));
                    }
                    SplitDir::Horizontal => {
                        assert_eq!((first.x, first.y, first.w, first.h), (10, 20, 204, 50));
                        assert_eq!((second.x, second.y, second.w, second.h), (10, 74, 204, 50));
                    }
                }
                assert!(closed.borrow().is_empty());
            }
        }
    }

    #[test]
    fn moving_nested_leaf_collapses_parent_and_preserves_sessions_and_ratios() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = nested(&closed);
        assert!(panes.move_pane(2, 3, SplitDir::Horizontal, false));
        assert_valid(&panes, &[1, 2, 3, 4]);
        assert_eq!(ids(&panes.root), [1, 3, 2, 4]);
        assert_eq!(panes.focused, 2);
        let moved = panes.root.find_pane(2).unwrap();
        assert_eq!(moved.terminal.id, 2);
        assert_eq!(moved.title, "/session/2");
        let PaneNode::Split { ratio, first, second, .. } = &panes.root else { panic!() };
        assert_eq!(*ratio, 0.35);
        assert!(matches!(&**first, PaneNode::Leaf(pane) if pane.id == 1));
        let PaneNode::Split { ratio, first, .. } = &**second else { panic!() };
        assert_eq!(*ratio, 0.7);
        assert!(matches!(&**first, PaneNode::Split { dir: SplitDir::Horizontal, .. }));
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn moving_next_to_sibling_rebuilds_only_that_split() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = nested(&closed);
        assert!(panes.move_pane(2, 1, SplitDir::Horizontal, true));
        assert_valid(&panes, &[1, 2, 3, 4]);
        assert_eq!(ids(&panes.root), [2, 1, 3, 4]);
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn invalid_moves_leave_sessions_layout_and_focus_untouched() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = nested(&closed);
        for (source, target) in [(2, 2), (9, 2), (2, 9)] {
            assert!(!panes.move_pane(source, target, SplitDir::Vertical, false));
        }
        assert_valid(&panes, &[1, 2, 3, 4]);
        assert_eq!(ids(&panes.root), [1, 2, 3, 4]);
        assert_eq!(panes.focused, 2);
        let PaneNode::Split { ratio, first, second, .. } = &panes.root else { panic!() };
        assert_eq!(*ratio, 0.35);
        assert!(matches!(&**first, PaneNode::Split { ratio: 0.25, .. }));
        assert!(matches!(&**second, PaneNode::Split { ratio: 0.7, .. }));
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn invalid_dock_keeps_subtree_and_sessions_owned_by_caller() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = Panes::new(pane(1, &closed));
        let mut carrier = Some(split(SplitDir::Vertical, 0.25, leaf(2, &closed), leaf(3, &closed)));
        assert!(!panes.root.dock_subtree(9, SplitDir::Horizontal, true, &mut carrier));
        assert_valid(&panes, &[1]);
        assert_eq!(ids(carrier.as_ref().unwrap()), [2, 3]);
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn docking_subtree_preserves_its_layout_and_sessions() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = Panes::new(pane(1, &closed));
        let mut carrier = Some(split(SplitDir::Vertical, 0.25, leaf(2, &closed), leaf(3, &closed)));
        assert!(panes.root.dock_subtree(1, SplitDir::Horizontal, true, &mut carrier));
        assert!(carrier.is_none());
        assert_valid(&panes, &[1, 2, 3]);
        assert_eq!(ids(&panes.root), [2, 3, 1]);
        let PaneNode::Split { first, .. } = &panes.root else { panic!() };
        assert!(matches!(&**first, PaneNode::Split { dir: SplitDir::Vertical, ratio: 0.25, .. }));
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn closing_focused_pane_repairs_focus_and_collapses_split() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = nested(&closed);
        assert!(panes.close(2));
        assert_valid(&panes, &[1, 3, 4]);
        assert_eq!(panes.focused, 1);
        assert_eq!(panes.title(), "/session/1");
        let PaneNode::Split { first, .. } = &panes.root else { panic!() };
        assert!(matches!(&**first, PaneNode::Leaf(pane) if pane.id == 1));
        assert_eq!(*closed.borrow(), [2]);
    }

    #[test]
    fn closing_background_pane_keeps_focus_and_invalid_close_is_noop() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = nested(&closed);
        assert!(panes.close(3));
        assert_valid(&panes, &[1, 2, 4]);
        assert_eq!(panes.focused, 2);
        assert!(!panes.close(9));
        assert_valid(&panes, &[1, 2, 4]);
        assert_eq!(panes.focused, 2);
        assert_eq!(*closed.borrow(), [3]);
    }

    #[test]
    fn closing_last_pane_leaves_empty_window_and_drops_session_once() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = nested(&closed);
        for id in [1, 2, 3] {
            assert!(panes.close(id));
        }
        assert_valid(&panes, &[4]);
        assert!(matches!(&panes.root, PaneNode::Leaf(pane) if pane.id == 4));
        assert!(panes.close(4));
        assert!(panes.is_empty());
        assert_valid(&panes, &[]);
        assert_eq!(panes.title(), "");
        assert!(!panes.close(4));
        drop(panes);
        assert_eq!(*closed.borrow(), [1, 2, 3, 4]);
    }

    #[test]
    fn closing_after_moves_and_dropping_window_releases_each_session_once() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = nested(&closed);
        assert!(panes.move_pane(2, 3, SplitDir::Horizontal, false));
        assert!(panes.close(3));
        assert_eq!(*closed.borrow(), [3]);
        assert_valid(&panes, &[1, 2, 4]);
        drop(panes);
        let mut all_closed = closed.borrow().clone();
        all_closed.sort_unstable();
        assert_eq!(all_closed, [1, 2, 3, 4]);
    }

    #[test]
    fn splitter_ratio_updates_follow_layout_order() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut panes = nested(&closed);
        panes.root.set_ratio(2, 0.6);
        let mut positions = Vec::new();
        let mut splitters = Vec::new();
        panes.root.layout(Rect { x: 0, y: 0, w: 104, h: 104 }, 4, &mut positions, &mut splitters);
        assert_eq!(splitters.iter().map(|hit| hit.index).collect::<Vec<_>>(), [0, 1, 2]);
        let PaneNode::Split { ratio, first, second, .. } = &panes.root else { panic!() };
        assert_eq!(*ratio, 0.35);
        assert!(matches!(&**first, PaneNode::Split { ratio: 0.25, .. }));
        assert!(matches!(&**second, PaneNode::Split { ratio: 0.6, .. }));
        assert_eq!(positions[2].1.w, 60);
        assert!(closed.borrow().is_empty());
    }
}
