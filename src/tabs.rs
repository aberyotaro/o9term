//! タブとペイン分割のデータ構造。
//! 各タブはペインの二分木を持ち、葉ごとに独立した PTY (Terminal) が動く。

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
    /// 一時的なプレースホルダ (木の組み替え中のみ)
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

    /// `id` の葉を分割し、新しいペインを後側 (右/下) に配置する。
    pub fn split_pane(&mut self, id: PaneId, dir: SplitDir, new_pane: &mut Option<Pane<T>>) -> bool {
        match self {
            PaneNode::Leaf(pane) if pane.id == id => {
                let Some(new_pane) = new_pane.take() else { return false };
                let old = std::mem::replace(self, PaneNode::Empty);
                *self = PaneNode::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(old),
                    second: Box::new(PaneNode::Leaf(new_pane)),
                };
                true
            }
            PaneNode::Split { first, second, .. } => {
                first.split_pane(id, dir, new_pane) || second.split_pane(id, dir, new_pane)
            }
            _ => false,
        }
    }

    /// `target` の葉を `dir` で分割し、`subtree` を `before` に応じて
    /// 前側 (左/上) または後側 (右/下) に挿入する。タブのドックに使う。
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

    /// `id` のペインを閉じ、残った木を縮約する。
    pub fn remove_pane(&mut self, id: PaneId) -> bool {
        self.take_pane(id).is_some()
    }

    pub fn pane_count(&self) -> usize {
        match self {
            PaneNode::Empty => 0,
            PaneNode::Leaf(_) => 1,
            PaneNode::Split { first, second, .. } => first.pane_count() + second.pane_count(),
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

    /// このノードが `id` の葉そのものかどうか (= 最後の1ペインか)。
    pub fn is_leaf_of(&self, id: PaneId) -> bool {
        matches!(self, PaneNode::Leaf(p) if p.id == id)
    }
}

pub struct Tab<T = Terminal> {
    pub root: PaneNode<T>,
    pub focused: PaneId,
}

impl<T> Tab<T> {
    pub fn new(pane: Pane<T>) -> Self {
        Self { focused: pane.id, root: PaneNode::Leaf(pane) }
    }

    /// タブの表示タイトル = フォーカス中ペインのタイトル (cwd のフルパス)。
    pub fn title(&self) -> &str {
        self.root
            .find_pane(self.focused)
            .or_else(|| self.root.first_pane())
            .map(|p| p.title.as_str())
            .unwrap_or("")
    }

    /// タブに表示する短いラベル = パスの末尾ディレクトリ名。
    pub fn label(&self) -> String {
        let title = self.title();
        match title.rsplit('/').next() {
            Some(last) if !last.is_empty() => last.to_string(),
            _ => title.to_string(),
        }
    }
}

/// ペインを同じタブ、または別タブの葉の隣へ移す。
/// 移動前に両端を検証するため、無効なドロップでも PTY を失わない。
pub fn move_pane<T>(
    tabs: &mut Vec<Tab<T>>,
    source: PaneId,
    target: PaneId,
    dir: SplitDir,
    before: bool,
) -> Option<usize> {
    if source == target {
        return None;
    }
    let from = tabs.iter().position(|tab| tab.root.contains(source))?;
    let mut to = tabs.iter().position(|tab| tab.root.contains(target))?;
    let pane = tabs[from].root.take_pane(source)?;
    if tabs[from].root.pane_count() == 0 {
        tabs.remove(from);
        if from < to {
            to -= 1;
        }
    } else if tabs[from].focused == source {
        tabs[from].focused = tabs[from].root.first_pane().unwrap().id;
    }
    let mut carrier = Some(PaneNode::Leaf(pane));
    let inserted = tabs[to].root.dock_subtree(target, dir, before, &mut carrier);
    // The destination leaf was validated and cannot be removed with another leaf.
    debug_assert!(inserted);
    tabs[to].focused = source;
    Some(to)
}

/// 分割ペインを新しいタブに戻す。挿入位置は移動前のタブの並びで指定する。
pub fn detach_pane<T>(tabs: &mut Vec<Tab<T>>, source: PaneId, position: usize) -> Option<usize> {
    let from = tabs.iter().position(|tab| tab.root.contains(source))?;
    if tabs[from].root.is_leaf_of(source) {
        return None;
    }
    let pane = tabs[from].root.take_pane(source)?;
    if tabs[from].focused == source {
        tabs[from].focused = tabs[from].root.first_pane().unwrap().id;
    }
    let position = position.min(tabs.len());
    tabs.insert(position, Tab::new(pane));
    Some(position)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A stand-in owned resource lets tree tests detect accidental terminal shutdown.
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

    fn nested(closed: &Rc<RefCell<Vec<PaneId>>>) -> Tab<Session> {
        Tab {
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
                let mut all = ids(first);
                all.extend(ids(second));
                all
            }
            PaneNode::Empty => panic!("Empty nodes must not survive a completed move"),
        }
    }

    fn assert_valid(tabs: &[Tab<Session>], expected: &[PaneId]) {
        let mut all = Vec::new();
        for tab in tabs {
            assert!(tab.root.contains(tab.focused), "focus must name a remaining pane");
            all.extend(ids(&tab.root));
        }
        all.sort_unstable();
        assert_eq!(all, expected, "a move must preserve all panes exactly once");
    }

    #[test]
    fn moving_nested_leaf_collapses_parent_and_preserves_sessions_and_ratios() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut tabs = vec![nested(&closed)];
        assert_eq!(move_pane(&mut tabs, 2, 3, SplitDir::Horizontal, false), Some(0));
        assert_valid(&tabs, &[1, 2, 3, 4]);
        assert_eq!(ids(&tabs[0].root), [1, 3, 2, 4]);
        assert_eq!(tabs[0].focused, 2);
        let moved = tabs[0].root.find_pane(2).unwrap();
        assert_eq!(moved.terminal.id, 2);
        assert_eq!(moved.title, "/session/2");
        let PaneNode::Split { ratio, first, second, .. } = &tabs[0].root else { panic!() };
        assert_eq!(*ratio, 0.35);
        assert!(first.is_leaf_of(1));
        let PaneNode::Split { ratio, first, .. } = &**second else { panic!() };
        assert_eq!(*ratio, 0.7);
        assert!(matches!(&**first, PaneNode::Split { dir: SplitDir::Horizontal, .. }));
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn moving_next_to_sibling_rebuilds_only_that_split() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut tabs = vec![nested(&closed)];
        assert_eq!(move_pane(&mut tabs, 2, 1, SplitDir::Horizontal, true), Some(0));
        assert_valid(&tabs, &[1, 2, 3, 4]);
        assert_eq!(ids(&tabs[0].root), [2, 1, 3, 4]);
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn moving_last_pane_between_tabs_removes_empty_tab_in_either_direction() {
        for source_first in [true, false] {
            let closed = Rc::new(RefCell::new(Vec::new()));
            let mut tabs = vec![Tab::new(pane(5, &closed)), nested(&closed)];
            if !source_first {
                tabs.reverse();
            }
            assert_eq!(move_pane(&mut tabs, 5, 3, SplitDir::Vertical, true), Some(0));
            assert_eq!(tabs.len(), 1);
            assert_valid(&tabs, &[1, 2, 3, 4, 5]);
            assert_eq!(tabs[0].focused, 5);
            assert_eq!(ids(&tabs[0].root), [1, 2, 5, 3, 4]);
            assert!(closed.borrow().is_empty());
        }
    }

    #[test]
    fn moving_focused_pane_to_another_tab_repairs_both_foci() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut tabs = vec![nested(&closed), Tab::new(pane(5, &closed))];
        assert_eq!(move_pane(&mut tabs, 2, 5, SplitDir::Vertical, false), Some(1));
        assert_valid(&tabs, &[1, 2, 3, 4, 5]);
        assert_eq!(tabs[0].focused, 1);
        assert_eq!(tabs[1].focused, 2);
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn detaching_to_tab_bar_preserves_session_and_repairs_source_focus() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut tabs = vec![nested(&closed)];
        assert_eq!(detach_pane(&mut tabs, 2, 0), Some(0));
        assert_valid(&tabs, &[1, 2, 3, 4]);
        assert_eq!(ids(&tabs[0].root), [2]);
        assert_eq!(ids(&tabs[1].root), [1, 3, 4]);
        assert_eq!(tabs[0].title(), "/session/2");
        assert_eq!(tabs[1].focused, 1);
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn detaching_background_pane_keeps_focus_and_clamps_insertion() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut tabs = vec![nested(&closed)];
        assert_eq!(detach_pane(&mut tabs, 3, 99), Some(1));
        assert_valid(&tabs, &[1, 2, 3, 4]);
        assert_eq!(tabs[0].focused, 2);
        assert_eq!(ids(&tabs[1].root), [3]);
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn invalid_moves_leave_all_sessions_and_layout_untouched() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut tabs = vec![nested(&closed), Tab::new(pane(5, &closed))];
        for (source, target) in [(2, 2), (9, 2), (2, 9)] {
            assert_eq!(move_pane(&mut tabs, source, target, SplitDir::Vertical, false), None);
        }
        assert_eq!(detach_pane(&mut tabs, 9, 0), None);
        assert_eq!(detach_pane(&mut tabs, 5, 0), None);
        assert_valid(&tabs, &[1, 2, 3, 4, 5]);
        assert_eq!(ids(&tabs[0].root), [1, 2, 3, 4]);
        assert_eq!(tabs[0].focused, 2);
        assert!(closed.borrow().is_empty());
    }

    #[test]
    fn closing_after_moves_shuts_down_only_the_chosen_session() {
        let closed = Rc::new(RefCell::new(Vec::new()));
        let mut tabs = vec![nested(&closed)];
        move_pane(&mut tabs, 2, 3, SplitDir::Horizontal, false);
        assert!(tabs[0].root.remove_pane(3));
        assert_eq!(*closed.borrow(), [3]);
        assert_valid(&tabs, &[1, 2, 4]);
        assert!(!tabs[0].root.remove_pane(9));
        assert_eq!(*closed.borrow(), [3]);
    }
}
