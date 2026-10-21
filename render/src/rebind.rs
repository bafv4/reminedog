//! キーの置き換えの状態：物理の押下ごとにゲームがどのキーかマウスのボタンを受け取るかと、
//! それぞれをゲームの中で押さえているもの。
//!
//! 押したときに 1 回だけ決める：解放は、そのときのルールがどうであれ、必ず押下がしたことに従う
//! ので、ゲームの中で押されたまま残るものはない。OS に依存しない純粋な処理：入力の振り分けが
//! 渡すはずのイベントを与え、各 OS のフックが返したものを届ける。

use reminedog_core::InputId;

/// 同時に使うルールの最大の数。
pub const MAX_REBINDS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Press,
    /// 押しているキーのオートリピート。
    Repeat,
    Release,
}

/// 物理のものの代わりにゲームが受け取るイベント。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    pub id: InputId,
    pub phase: Phase,
}

/// キーかマウスのボタンのイベントをどうするか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// イベントをそのままゲームに渡す。
    Forward,
    /// ゲームに見せてはならない。
    Consume,
    /// 代わりにこれをゲームに渡す。
    Send(Output),
}

/// 元のキーの押下がしたこと。解放まで覚える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Latch {
    /// ゲームは押下をそのまま受け取った。
    Passthrough,
    /// ゲームは押下をこのキーかボタンとして受け取った。
    Rebound(InputId),
    /// 置き換えていて、ウィンドウがフォーカスを失ったときに出力を離した。元のキー自身の解放は
    /// まだ来る。
    Orphaned,
}

/// 置き換えがゲームから押さえているものと取っているもの（ゲームが押していると読むキーの各 OS の
/// 表と、F3+C の確認のため）。id が両方にある（A と B を入れ替えて両方押している）ときは
/// `held` が勝つ：ゲームでは押されている。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebindState {
    /// 押さえているルールごとの (元のキー, 出力)：出力はゲームの中で下りている。
    pub held: Vec<(InputId, InputId)>,
    /// ゲームが離していると見なければならない元のキー：`held` のものと、フォーカスを失ったときに
    /// 出力を離したが、自分自身の解放がまだ来るもの。
    pub taken: Vec<InputId>,
    /// ルールが出力するキーとボタンのうち、ゲームがそのまま押されたものとして受け取り、まだ離されて
    /// いないもの。解放が来ないと（26.x はワールドの読み込み中に入力のイベントを捨てる）、その id
    /// へのルール自身の押下が飲み込まれてしまう。[`crate::InputRouter::forget_passthrough`] を参照。
    pub passthrough: Vec<InputId>,
}

impl RebindState {
    /// 置き換えがゲームの中で `id` を押さえているか。
    pub fn holds(&self, id: InputId) -> bool {
        self.held.iter().any(|&(_, output)| output == id)
    }

    /// 物理的には押しているかもしれないが、ゲームが `id` を離していると見なければならないか。
    pub fn takes(&self, id: InputId) -> bool {
        !self.holds(id) && self.taken.contains(&id)
    }
}

pub(crate) struct Rebinder {
    /// (元のキー, 出力)。元のキーごとに多くても 1 つ。
    rules: Vec<(InputId, InputId)>,
    latch: Vec<(InputId, Latch)>,
    /// 届けた各 id をゲームの中で押さえている数：置き換えた出力と、そのまま通した物理の押下
    /// （物理の W と X → W は 1 回の押下と 1 回の解放を共有する）。
    held: Vec<(InputId, u8)>,
}

impl Rebinder {
    pub(crate) const fn new() -> Self {
        Self {
            rules: Vec::new(),
            latch: Vec::new(),
            held: Vec::new(),
        }
    }

    /// 後の押下を決めるルール。押さえている押下は、したことのまま。同じ元のキーのルールは最初の
    /// ものが勝ち、多くても [`MAX_REBINDS`] 個を持つ。
    pub(crate) fn set_rules(&mut self, rules: Vec<(InputId, InputId)>) {
        self.rules.clear();
        for (source, output) in rules {
            if self.rules.len() < MAX_REBINDS && self.rule(source).is_none() {
                self.rules.push((source, output));
            }
        }
    }

    pub(crate) fn rules(&self) -> &[(InputId, InputId)] {
        &self.rules
    }

    fn rule(&self, source: InputId) -> Option<InputId> {
        self.rules
            .iter()
            .find(|&&(s, _)| s == source)
            .map(|&(_, output)| output)
    }

    /// ルールがなく、何も押さえていない：どのイベントもそのまま通る。
    fn bypass(&self) -> bool {
        self.rules.is_empty() && self.latch.is_empty()
    }

    fn latch_of(&self, source: InputId) -> Option<Latch> {
        self.latch
            .iter()
            .find(|&&(s, _)| s == source)
            .map(|&(_, latch)| latch)
    }

    fn unlatch(&mut self, source: InputId) -> Option<Latch> {
        let i = self.latch.iter().position(|&(s, _)| s == source)?;
        Some(self.latch.swap_remove(i).1)
    }

    /// 元のキーの押下が別のキーとしてゲームに届いた（か、フォーカスを失う前に届いた）か：その
    /// リピート、文字、解放はゲームのものではない。
    pub(crate) fn takes(&self, source: InputId) -> bool {
        matches!(
            self.latch_of(source),
            Some(Latch::Rebound(_) | Latch::Orphaned)
        )
    }

    /// `id` を押さえる数を 1 つ増やす。新しい数。
    fn hold(&mut self, id: InputId) -> u8 {
        match self.held.iter_mut().find(|(i, _)| *i == id) {
            Some((_, count)) => {
                *count = count.saturating_add(1);
                *count
            }
            None => {
                self.held.push((id, 1));
                1
            }
        }
    }

    /// `id` を押さえる数を 1 つ減らす。新しい数。
    fn unhold(&mut self, id: InputId) -> u8 {
        let Some(i) = self.held.iter().position(|&(i, _)| i == id) else {
            return 0;
        };
        let count = &mut self.held[i].1;
        *count = count.saturating_sub(1);
        let left = *count;
        if left == 0 {
            self.held.swap_remove(i);
        }
        left
    }

    /// 前の押下が記録されている元のキーのイベントについて、router 自身の処理（ホットキー、UI）が
    /// 見る前の最初の判断。`None` ならイベントを router に任せる。そのときは古い記録を捨て、押下を
    /// 決め直す。
    ///
    /// `active`：今ルールが効く（ゲーム中で、UI を閉じている）。`stateless`：OS はこのキーの
    /// オートリピートを押下として送り、フォーカスを失っても解放を送らない（GLFW の key -1）。
    pub(crate) fn latched(
        &mut self,
        source: InputId,
        phase: Phase,
        active: bool,
        stateless: bool,
    ) -> Option<Delivery> {
        let latch = self.latch_of(source)?;
        match (phase, latch) {
            (Phase::Press, Latch::Rebound(output)) if stateless => {
                Some(self.repeat(output, active))
            }
            (Phase::Press, _) => {
                // 前の押下から解放のない押下：解放がなくなったか、解放のないキーのオートリピート。
                // 決め直す。ゲームは置き換えた出力を、その出力の次の解放まで押さえたままにする。
                self.unlatch(source);
                let held = match latch {
                    Latch::Rebound(output) => Some(output),
                    Latch::Passthrough => Some(source),
                    Latch::Orphaned => None,
                };
                if let Some(id) = held {
                    self.unhold(id);
                }
                None
            }
            (Phase::Repeat, Latch::Rebound(output)) => Some(self.repeat(output, active)),
            (Phase::Repeat | Phase::Release, Latch::Orphaned) => {
                if phase == Phase::Release {
                    self.unlatch(source);
                }
                Some(Delivery::Consume)
            }
            (Phase::Release, Latch::Rebound(output)) => {
                self.unlatch(source);
                Some(self.let_go(output))
            }
            (Phase::Repeat | Phase::Release, Latch::Passthrough) => None,
        }
    }

    /// 置き換えた元のキーのリピート：ルールが効いていて、出力がキーなら、出力のリピート
    /// （マウスのボタンはリピートしない。リピートはそれを離してしまう）。
    fn repeat(&self, output: InputId, active: bool) -> Delivery {
        if active && matches!(output, InputId::Key(_)) {
            Delivery::Send(Output {
                id: output,
                phase: Phase::Repeat,
            })
        } else {
            Delivery::Consume
        }
    }

    /// 置き換えた出力を押さえる数を 1 つ減らす。最後だったなら、その解放。
    fn let_go(&mut self, output: InputId) -> Delivery {
        if self.unhold(output) == 0 {
            Delivery::Send(Output {
                id: output,
                phase: Phase::Release,
            })
        } else {
            Delivery::Consume
        }
    }

    /// router が渡すはずの押下：`active` で、この元のキーのルールがあれば置き換え、なければ
    /// そのまま通す。どちらでも、ゲームが押下を受け取るのは、その id をほかの元のキーがまだ
    /// 押さえていないときだけ。
    pub(crate) fn press(&mut self, source: InputId, active: bool) -> Delivery {
        if self.bypass() {
            return Delivery::Forward;
        }
        self.unlatch(source);
        match self.rule(source).filter(|_| active) {
            Some(output) => {
                self.latch.push((source, Latch::Rebound(output)));
                if self.hold(output) == 1 {
                    Delivery::Send(Output {
                        id: output,
                        phase: Phase::Press,
                    })
                } else {
                    Delivery::Consume
                }
            }
            None => {
                self.latch.push((source, Latch::Passthrough));
                if self.hold(source) == 1 {
                    Delivery::Forward
                } else {
                    Delivery::Consume
                }
            }
        }
    }

    /// router が渡すはずの、[`latched`] が取らなかった元のキーの解放（そのまま通したか、何かを
    /// 記録する前に押した）。
    ///
    /// [`latched`]: Self::latched
    pub(crate) fn release(&mut self, source: InputId) -> Delivery {
        match self.unlatch(source) {
            Some(Latch::Passthrough) => {
                if self.unhold(source) == 0 {
                    Delivery::Forward
                } else {
                    Delivery::Consume
                }
            }
            // `latched` がこれを決める。飛ばしたときだけここに来る。
            Some(Latch::Rebound(output)) => self.let_go(output),
            Some(Latch::Orphaned) => Delivery::Consume,
            // ルールが id を押さえている間に router が取った押下（たとえばメニューが開いていた）：
            // ルール自身の解放がそれを離す。
            None if self.rule_holds(source) => Delivery::Consume,
            None => Delivery::Forward,
        }
    }

    /// 押さえているルールが、ゲームの中で `id` を下ろしているか。
    pub(crate) fn rule_holds(&self, id: InputId) -> bool {
        self.latch
            .iter()
            .any(|&(_, latch)| latch == Latch::Rebound(id))
    }

    /// ゲームがそのまま受け取ったが解放が来なかった押下を、ゲームに何も渡さずに忘れる：OS が
    /// キーかボタンが離れているのを見つけ、ゲームは自分で離した（26.x はワールドの読み込みか
    /// 保存の間に入力のイベントを捨て、画面が変わると自分のキーの状態を戻す）。`source` にそういう
    /// 押下があったかを返す。
    pub(crate) fn forget_passthrough(&mut self, source: InputId) -> bool {
        if self.latch_of(source) != Some(Latch::Passthrough) {
            return false;
        }
        self.unlatch(source);
        self.unhold(source);
        true
    }

    /// ウィンドウがフォーカスを失った：何も押さえなくなった出力について、ゲームがいま受け取るべき
    /// 解放。`platform_releases(source)`：OS はその元のキーの解放をまだ送る（GLFW は key -1 には、
    /// SDL3 はマウスのボタンには送らない）。
    ///
    /// 解放が来る置き換えた元のキーは孤児になる（その解放はそのとき捨てる）。そのまま通したものは
    /// 残る（その解放はゲームに行く）。解放が来ない記録は捨て、それだけが押さえていた id も離す。
    pub(crate) fn focus_lost(
        &mut self,
        platform_releases: impl Fn(InputId) -> bool,
    ) -> Vec<Output> {
        let mut releases = Vec::new();
        for (source, latch) in std::mem::take(&mut self.latch) {
            let comes = platform_releases(source);
            let let_go = match latch {
                Latch::Rebound(output) => Some(output),
                Latch::Passthrough if !comes => Some(source),
                Latch::Passthrough | Latch::Orphaned => None,
            };
            if let Some(id) = let_go
                && self.unhold(id) == 0
            {
                releases.push(Output {
                    id,
                    phase: Phase::Release,
                });
            }
            if comes {
                let kept = match latch {
                    Latch::Passthrough => Latch::Passthrough,
                    Latch::Rebound(_) | Latch::Orphaned => Latch::Orphaned,
                };
                self.latch.push((source, kept));
            }
        }
        releases
    }

    /// 解放は来なかったが、元のキーは離された（OS が本当のキーの状態を確かめた）：解放と同じように、
    /// ゲームに渡すイベントがあればそれも。
    pub(crate) fn release_source(&mut self, source: InputId) -> Option<Output> {
        let id = match self.unlatch(source)? {
            Latch::Rebound(output) => output,
            Latch::Passthrough => source,
            Latch::Orphaned => return None,
        };
        (self.unhold(id) == 0).then_some(Output {
            id,
            phase: Phase::Release,
        })
    }

    pub(crate) fn state(&self) -> RebindState {
        let mut state = RebindState::default();
        for &(source, latch) in &self.latch {
            match latch {
                Latch::Rebound(output) => {
                    state.held.push((source, output));
                    state.taken.push(source);
                }
                Latch::Orphaned => state.taken.push(source),
                Latch::Passthrough => {
                    if self.rules.iter().any(|&(_, output)| output == source) {
                        state.passthrough.push(source);
                    }
                }
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Delivery::{Consume, Forward};
    use Phase::{Press, Release, Repeat};

    const A: InputId = InputId::Key(4);
    const B: InputId = InputId::Key(5);
    const C: InputId = InputId::Key(6);
    const W: InputId = InputId::Key(26);
    const X: InputId = InputId::Key(27);
    const F3: InputId = InputId::Key(60);
    const BACKSPACE: InputId = InputId::Key(42);
    /// A Japanese key (ろ), key -1 under GLFW.
    const RO: InputId = InputId::Key(135);
    const LEFT: InputId = InputId::Mouse(1);
    const MOUSE4: InputId = InputId::Mouse(4);
    const MOUSE5: InputId = InputId::Mouse(5);

    fn rebinder(rules: &[(InputId, InputId)]) -> Rebinder {
        let mut r = Rebinder::new();
        r.set_rules(rules.to_vec());
        r
    }

    /// router の自身の処理が全部を渡すときと同じように。
    fn event(r: &mut Rebinder, id: InputId, phase: Phase, active: bool) -> Delivery {
        stateless_event(r, id, phase, active, false)
    }

    fn stateless_event(
        r: &mut Rebinder,
        id: InputId,
        phase: Phase,
        active: bool,
        stateless: bool,
    ) -> Delivery {
        if let Some(delivery) = r.latched(id, phase, active, stateless) {
            return delivery;
        }
        match phase {
            Press => r.press(id, active),
            Repeat => Delivery::Forward,
            Release => r.release(id),
        }
    }

    fn send(id: InputId, phase: Phase) -> Delivery {
        Delivery::Send(Output { id, phase })
    }

    fn output(id: InputId, phase: Phase) -> Output {
        Output { id, phase }
    }

    #[test]
    fn without_rules_everything_goes_on_untracked() {
        let mut r = rebinder(&[]);
        for phase in [Press, Repeat, Release, Press] {
            assert_eq!(event(&mut r, W, phase, true), Forward, "{phase:?}");
        }
        assert!(r.latch.is_empty() && r.held.is_empty());
        assert!(r.focus_lost(|_| true).is_empty());
        assert_eq!(event(&mut r, W, Release, true), Forward);
    }

    #[test]
    fn a_rule_sends_its_output_while_active() {
        let mut r = rebinder(&[(MOUSE4, F3)]);
        assert_eq!(event(&mut r, MOUSE4, Press, true), send(F3, Press));
        assert!(r.takes(MOUSE4));
        assert_eq!(event(&mut r, MOUSE4, Release, true), send(F3, Release));
        assert!(!r.takes(MOUSE4));
        // ゲーム中ではない：ボタンはそのままゲームのもの。
        assert_eq!(event(&mut r, MOUSE4, Press, false), Forward);
        assert_eq!(event(&mut r, MOUSE4, Release, true), Forward);
        // ほかのキーはそのまま通る。
        assert_eq!(event(&mut r, W, Press, true), Forward);
        assert_eq!(event(&mut r, W, Repeat, true), Forward);
        assert_eq!(event(&mut r, W, Release, true), Forward);
        assert!(r.latch.is_empty() && r.held.is_empty());
    }

    #[test]
    fn the_release_follows_the_press_whatever_the_rules_became() {
        // 別のキーに変えた。
        let mut r = rebinder(&[(A, F3)]);
        assert_eq!(event(&mut r, A, Press, true), send(F3, Press));
        r.set_rules(vec![(A, BACKSPACE)]);
        assert_eq!(event(&mut r, A, Repeat, true), send(F3, Repeat));
        assert_eq!(event(&mut r, A, Release, true), send(F3, Release));
        assert_eq!(event(&mut r, A, Press, true), send(BACKSPACE, Press));
        // 消して、置き換えをオフにした（ルールなし）。
        r.set_rules(vec![]);
        assert_eq!(event(&mut r, A, Release, true), send(BACKSPACE, Release));
        assert_eq!(event(&mut r, A, Press, true), Forward);
        // マウスのボタンに切り替えた。
        let mut r = rebinder(&[(A, F3)]);
        event(&mut r, A, Press, true);
        r.set_rules(vec![(A, LEFT)]);
        assert_eq!(event(&mut r, A, Release, false), send(F3, Release));
        assert_eq!(event(&mut r, A, Press, true), send(LEFT, Press));
        r.set_rules(vec![(A, F3)]);
        assert_eq!(event(&mut r, A, Release, true), send(LEFT, Release));
    }

    #[test]
    fn a_press_not_rebound_releases_as_itself() {
        // 画面で押し、ゲーム中に離した。
        let mut r = rebinder(&[(A, F3)]);
        assert_eq!(event(&mut r, A, Press, false), Forward);
        assert_eq!(event(&mut r, A, Repeat, true), Forward);
        assert_eq!(event(&mut r, A, Release, true), Forward);
        // ルールができる前に押した。
        let mut r = rebinder(&[(B, C)]);
        assert_eq!(event(&mut r, A, Press, true), Forward);
        r.set_rules(vec![(A, F3)]);
        assert_eq!(event(&mut r, A, Release, true), Forward);
        // 何かを追う前に押した。
        let mut r = rebinder(&[(A, F3)]);
        assert_eq!(event(&mut r, A, Release, true), Forward);
    }

    #[test]
    fn repeats_follow_the_output_only_while_active() {
        let mut r = rebinder(&[(X, W), (C, LEFT)]);
        assert_eq!(event(&mut r, X, Press, true), send(W, Press));
        assert_eq!(event(&mut r, X, Repeat, true), send(W, Repeat));
        // 押している間に UI か画面が開いた：出力は下りたまま、何もリピートしない。
        assert_eq!(event(&mut r, X, Repeat, false), Consume);
        assert_eq!(event(&mut r, X, Release, false), send(W, Release));
        // マウスのボタンはリピートしない（リピートはそれを離してしまう）。
        assert_eq!(event(&mut r, C, Press, true), send(LEFT, Press));
        assert_eq!(event(&mut r, C, Repeat, true), Consume);
        assert_eq!(event(&mut r, C, Release, true), send(LEFT, Release));
    }

    #[test]
    fn mouse_to_key() {
        let mut r = rebinder(&[(MOUSE5, C)]);
        assert_eq!(event(&mut r, MOUSE5, Press, true), send(C, Press));
        assert_eq!(r.state().held, [(MOUSE5, C)]);
        assert_eq!(event(&mut r, MOUSE5, Release, true), send(C, Release));
        assert_eq!(r.state(), RebindState::default());
    }

    #[test]
    fn two_sources_on_one_output_in_either_order() {
        for (first, last) in [(A, B), (B, A)] {
            let mut r = rebinder(&[(A, F3), (B, F3)]);
            assert_eq!(event(&mut r, A, Press, true), send(F3, Press));
            assert_eq!(event(&mut r, B, Press, true), Consume);
            assert_eq!(event(&mut r, first, Release, true), Consume);
            assert!(r.state().holds(F3));
            assert!(!r.state().taken.contains(&first));
            assert_eq!(event(&mut r, last, Release, true), send(F3, Release));
            assert_eq!(r.state(), RebindState::default());
        }
    }

    #[test]
    fn a_physical_key_and_a_rule_share_its_press() {
        // 物理の W、次に X → W：両方を離したときだけ W が上がる。
        let mut r = rebinder(&[(X, W)]);
        assert_eq!(event(&mut r, W, Press, true), Forward);
        assert_eq!(event(&mut r, X, Press, true), Consume);
        assert_eq!(event(&mut r, X, Release, true), Consume);
        assert_eq!(event(&mut r, W, Repeat, true), Forward);
        assert_eq!(event(&mut r, W, Release, true), Forward);
        // 逆の順。
        assert_eq!(event(&mut r, X, Press, true), send(W, Press));
        assert_eq!(event(&mut r, W, Press, true), Consume);
        assert_eq!(event(&mut r, W, Release, true), Consume);
        assert!(r.state().holds(W));
        assert_eq!(event(&mut r, X, Release, true), send(W, Release));
    }

    #[test]
    fn swapped_keys() {
        let mut r = rebinder(&[(A, B), (B, A)]);
        assert_eq!(event(&mut r, A, Press, true), send(B, Press));
        assert_eq!(event(&mut r, B, Press, true), send(A, Press));
        let state = r.state();
        // どちらも取られた元のキーで、押さえている出力：held が勝つ。
        assert!(state.holds(A) && state.holds(B));
        assert!(!state.takes(A) && !state.takes(B));
        assert_eq!(event(&mut r, A, Release, true), send(B, Release));
        let state = r.state();
        assert!(state.holds(A) && !state.holds(B));
        assert!(state.takes(B), "B is still held as A");
        assert_eq!(event(&mut r, B, Release, true), send(A, Release));
    }

    #[test]
    fn focus_loss_lets_go_of_outputs_and_drops_the_late_releases() {
        let mut r = rebinder(&[(X, F3), (MOUSE4, C)]);
        assert_eq!(event(&mut r, W, Press, true), Forward);
        assert_eq!(event(&mut r, X, Press, true), send(F3, Press));
        assert_eq!(event(&mut r, MOUSE4, Press, true), send(C, Press));
        // GLFW：フォーカスのコールバックの後に、どのキーとボタンも解放を受け取る。
        let mut releases = r.focus_lost(|_| true);
        releases.sort_by_key(|o| o.id);
        assert_eq!(releases, [output(C, Release), output(F3, Release)]);
        assert_eq!(r.state().taken.len(), 2, "orphaned sources stay up");
        assert!(r.state().held.is_empty());
        // OS 自身の解放：ただのキーのものはゲームに行く。
        assert_eq!(event(&mut r, W, Release, true), Forward);
        assert_eq!(event(&mut r, X, Release, true), Consume);
        assert_eq!(event(&mut r, MOUSE4, Release, true), Consume);
        assert!(r.latch.is_empty() && r.held.is_empty());
    }

    #[test]
    fn focus_loss_without_rules_keeps_plain_releases() {
        // ルールはあるが何も押さえていない：W はやはり自分の解放で離されなければならない。
        let mut r = rebinder(&[(X, F3)]);
        event(&mut r, W, Press, true);
        assert!(r.focus_lost(|_| true).is_empty());
        assert_eq!(event(&mut r, W, Release, true), Forward);
    }

    #[test]
    fn a_source_whose_release_never_comes_works_after_focus_loss() {
        // SDL3 はフォーカスを失ってもマウスのボタンを離さない。
        let sdl = |id: InputId| matches!(id, InputId::Key(_));
        let mut r = rebinder(&[(MOUSE4, F3)]);
        assert_eq!(event(&mut r, MOUSE4, Press, true), send(F3, Press));
        assert_eq!(event(&mut r, LEFT, Press, true), Forward);
        let mut releases = r.focus_lost(sdl);
        releases.sort_by_key(|o| o.id);
        // ゲームがそのまま受け取った左のボタンも離す：ほかに離すものはない。
        assert_eq!(releases, [output(F3, Release), output(LEFT, Release)]);
        assert!(r.latch.is_empty() && r.held.is_empty());
        // ゲームに戻った：次の押下は効く。
        assert_eq!(event(&mut r, MOUSE4, Press, true), send(F3, Press));
        assert_eq!(event(&mut r, MOUSE4, Release, true), send(F3, Release));
    }

    #[test]
    fn an_orphaned_source_pressed_again_is_decided_anew() {
        // 孤児の解放がなくなった（例：IME が食べた）。
        let mut r = rebinder(&[(X, F3)]);
        event(&mut r, X, Press, true);
        assert_eq!(r.focus_lost(|_| true), [output(F3, Release)]);
        assert_eq!(event(&mut r, X, Repeat, true), Consume);
        assert_eq!(event(&mut r, X, Press, true), send(F3, Press));
        assert_eq!(event(&mut r, X, Release, true), send(F3, Release));
    }

    #[test]
    fn a_missed_release_does_not_eat_the_next_press() {
        let mut r = rebinder(&[(X, F3)]);
        assert_eq!(event(&mut r, X, Press, true), send(F3, Press));
        // X の解放がなくなった。次の解放で直るまで、ゲームの中で F3 は下りたまま。
        assert_eq!(event(&mut r, X, Press, true), send(F3, Press));
        assert_eq!(event(&mut r, X, Release, true), send(F3, Release));
        assert!(r.held.is_empty());
        // ただのキーでも同じ。
        assert_eq!(event(&mut r, W, Press, true), Forward);
        assert_eq!(event(&mut r, W, Press, true), Forward);
        assert_eq!(event(&mut r, W, Release, true), Forward);
        assert!(r.held.is_empty());
    }

    #[test]
    fn a_stateless_key_repeats_with_presses() {
        // GLFW の key -1（日本語のキー）：オートリピートが押下として来る。
        let mut r = rebinder(&[(RO, W)]);
        assert_eq!(
            stateless_event(&mut r, RO, Press, true, true),
            send(W, Press)
        );
        assert_eq!(
            stateless_event(&mut r, RO, Press, true, true),
            send(W, Repeat)
        );
        assert_eq!(stateless_event(&mut r, RO, Press, false, true), Consume);
        assert_eq!(
            stateless_event(&mut r, RO, Release, true, true),
            send(W, Release)
        );
        // GLFW はフォーカスを失ってもその解放を送らない：孤児にせず捨てる。
        stateless_event(&mut r, RO, Press, true, true);
        let glfw = |id: InputId| id != RO;
        assert_eq!(r.focus_lost(glfw), [output(W, Release)]);
        assert!(r.latch.is_empty());
        assert_eq!(
            stateless_event(&mut r, RO, Press, true, true),
            send(W, Press)
        );
    }

    #[test]
    fn set_rules_keeps_held_presses() {
        let mut r = rebinder(&[(X, F3)]);
        event(&mut r, X, Press, true);
        event(&mut r, W, Press, true);
        r.set_rules(vec![(W, C)]);
        assert_eq!(event(&mut r, X, Release, true), send(F3, Release));
        assert_eq!(event(&mut r, W, Release, true), Forward);
        assert_eq!(event(&mut r, W, Press, true), send(C, Press));
    }

    #[test]
    fn rules_are_unique_by_source_and_limited() {
        let mut r = rebinder(&[(X, F3), (X, C)]);
        assert_eq!(r.rules(), [(X, F3)]);
        let many: Vec<_> = (4..60).map(|sc| (InputId::Key(sc), F3)).collect();
        r.set_rules(many);
        assert_eq!(r.rules().len(), MAX_REBINDS);
    }

    #[test]
    fn release_source_lets_go_as_a_release_would() {
        let mut r = rebinder(&[(X, F3), (B, F3)]);
        event(&mut r, X, Press, true);
        event(&mut r, B, Press, true);
        assert_eq!(r.release_source(X), None, "B still holds F3");
        assert_eq!(r.release_source(B), Some(output(F3, Release)));
        assert_eq!(r.release_source(B), None);
        // 遅れた解放は、もう記録されていない。
        assert_eq!(event(&mut r, X, Release, true), Forward);
        event(&mut r, W, Press, true);
        assert_eq!(r.release_source(W), Some(output(W, Release)));
    }

    #[test]
    fn a_release_of_an_id_a_rule_holds_is_the_rules() {
        // X → W が W を押さえている間に、router が W の押下を取った（メニューが開いていた）。
        let mut r = rebinder(&[(X, W)]);
        assert_eq!(event(&mut r, X, Press, true), send(W, Press));
        assert_eq!(r.release(W), Consume, "X still holds W");
        assert_eq!(event(&mut r, X, Release, true), send(W, Release));
        // もう何も押さえていない。
        assert_eq!(r.release(W), Forward);
        assert_eq!(r.release(X), Forward);
    }

    #[test]
    fn a_stale_passthrough_press_is_forgotten_without_a_release() {
        const H: InputId = InputId::Key(11);
        // 26.x は、ワールドの読み込みを始めたクリックの解放を捨てた。
        let mut r = rebinder(&[(H, LEFT)]);
        assert_eq!(event(&mut r, LEFT, Press, false), Forward);
        assert_eq!(r.state().passthrough, [LEFT]);
        assert!(r.state().held.is_empty());
        // 並べるのはルールの出力の押下だけ。
        assert_eq!(event(&mut r, W, Press, false), Forward);
        assert_eq!(r.state().passthrough, [LEFT]);
        assert!(r.forget_passthrough(LEFT));
        assert!(!r.forget_passthrough(LEFT), "only once");
        assert!(r.state().passthrough.is_empty());
        // ルールの押下がまたゲームに届く。
        assert_eq!(event(&mut r, H, Press, true), send(LEFT, Press));
        assert!(!r.forget_passthrough(H), "a rebound press is not forgotten");
        assert_eq!(event(&mut r, H, Release, true), send(LEFT, Release));
        // 忘れた押下の遅れた解放はゲームのもの。
        assert_eq!(event(&mut r, LEFT, Release, true), Forward);
        assert_eq!(event(&mut r, W, Release, true), Forward);
        assert!(r.latch.is_empty() && r.held.is_empty());
    }

    #[test]
    fn state_lists_held_outputs_and_taken_sources() {
        let mut r = rebinder(&[(MOUSE4, F3), (MOUSE5, C)]);
        event(&mut r, MOUSE4, Press, true);
        event(&mut r, W, Press, true);
        let state = r.state();
        assert_eq!(state.held, [(MOUSE4, F3)]);
        assert_eq!(state.taken, [MOUSE4]);
        assert!(
            state.holds(F3) && !state.holds(W),
            "plain keys are the platform's"
        );
        assert!(state.takes(MOUSE4));
    }
}
