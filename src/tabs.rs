//! タブとペイン分割のデータ構造。
//! 各タブはペインの二分木を持ち、葉ごとに独立した PTY (Terminal) が動く。

use crate::renderer::Rect;
use crate::term::{PaneId, Terminal};

pub struct Pane {
    pub id: PaneId,
    pub terminal: Terminal,
    pub title: String,
}

/// スプリッター(仕切り線)の向き。Vertical = 縦線で左右に分割。
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SplitDir {
    Vertical,
    Horizontal,
}

pub enum PaneNode {
    /// 一時的なプレースホルダ (木の組み替え中のみ)
    Empty,
    Leaf(Pane),
    Split { dir: SplitDir, ratio: f32, first: Box<PaneNode>, second: Box<PaneNode> },
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

impl PaneNode {
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
    pub fn split_pane(&mut self, id: PaneId, dir: SplitDir, new_pane: &mut Option<Pane>) -> bool {
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

    /// `id` の葉を取り除き、親の分割ノードをもう片方の子で置き換える。
    /// ルートが対象の葉そのものである場合は呼び出し側でタブごと閉じること。
    pub fn remove_pane(&mut self, id: PaneId) -> bool {
        if let PaneNode::Split { first, second, .. } = self {
            if matches!(&**first, PaneNode::Leaf(p) if p.id == id) {
                let other = std::mem::replace(&mut **second, PaneNode::Empty);
                *self = other;
                return true;
            }
            if matches!(&**second, PaneNode::Leaf(p) if p.id == id) {
                let other = std::mem::replace(&mut **first, PaneNode::Empty);
                *self = other;
                return true;
            }
            first.remove_pane(id) || second.remove_pane(id)
        } else {
            false
        }
    }

    pub fn find_pane(&self, id: PaneId) -> Option<&Pane> {
        match self {
            PaneNode::Leaf(pane) if pane.id == id => Some(pane),
            PaneNode::Split { first, second, .. } => {
                first.find_pane(id).or_else(|| second.find_pane(id))
            }
            _ => None,
        }
    }

    pub fn find_pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        match self {
            PaneNode::Leaf(pane) if pane.id == id => Some(pane),
            PaneNode::Split { first, second, .. } => {
                first.find_pane_mut(id).or_else(|| second.find_pane_mut(id))
            }
            _ => None,
        }
    }

    pub fn first_pane(&self) -> Option<&Pane> {
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

    /// このノードが `id` の葉そのものかどうか (= 最後の1ペインか)。
    pub fn is_leaf_of(&self, id: PaneId) -> bool {
        matches!(self, PaneNode::Leaf(p) if p.id == id)
    }
}

pub struct Tab {
    pub root: PaneNode,
    pub focused: PaneId,
}

impl Tab {
    pub fn new(pane: Pane) -> Self {
        Self { focused: pane.id, root: PaneNode::Leaf(pane) }
    }

    /// タブの表示タイトル = フォーカス中ペインのタイトル。
    pub fn title(&self) -> &str {
        self.root
            .find_pane(self.focused)
            .or_else(|| self.root.first_pane())
            .map(|p| p.title.as_str())
            .unwrap_or("")
    }
}
