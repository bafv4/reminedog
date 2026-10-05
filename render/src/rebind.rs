//! The key rebinding's state: which key or mouse button the game gets for each physical
//! press, and what keeps each of them held in the game.
//!
//! Decided once, at the press: a release always follows what its press did, whatever the
//! rules are by then, so nothing stays held in the game. Pure and platform-neutral; the input
//! router feeds it the events it would forward and the platform hooks deliver what it returns.

use reminedog_core::InputId;

/// The most rules in use at once.
pub const MAX_REBINDS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Press,
    /// Auto-repeat of a held key.
    Repeat,
    Release,
}

/// An event the game gets in place of the physical one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    pub id: InputId,
    pub phase: Phase,
}

/// What to do with a key or mouse button event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Pass the event on to the game as it is.
    Forward,
    /// The game must not see it.
    Consume,
    /// Give the game this instead.
    Send(Output),
}

/// What a source's press did, kept until its release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Latch {
    /// The game got the press as it was.
    Passthrough,
    /// The game got the press as this key or button.
    Rebound(InputId),
    /// Was rebound, and the output was let go when the window lost focus; the source's own
    /// release is still to come.
    Orphaned,
}

/// What the rebinding holds and takes from the game, for the platform's table of keys the game
/// reads as held, and F3+C's checks. Where an id is in both (A and B swapped, both held),
/// `held` wins: the game has it down.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebindState {
    /// (source, output) of each held rule: the output is down in the game.
    pub held: Vec<(InputId, InputId)>,
    /// Sources the game must see as up: those of `held`, and those whose output was let go on
    /// focus loss while their own release is still to come.
    pub taken: Vec<InputId>,
    /// Keys and buttons a rule outputs that the game got pressed as they were and that are not
    /// released yet. Should a release never come (26.x drops input events while it loads a
    /// world), the rule's own presses of the id would be swallowed; see
    /// [`crate::InputRouter::forget_passthrough`].
    pub passthrough: Vec<InputId>,
}

impl RebindState {
    /// Whether the rebinding holds `id` down in the game.
    pub fn holds(&self, id: InputId) -> bool {
        self.held.iter().any(|&(_, output)| output == id)
    }

    /// Whether the game must see `id` as up although it may be physically held.
    pub fn takes(&self, id: InputId) -> bool {
        !self.holds(id) && self.taken.contains(&id)
    }
}

pub(crate) struct Rebinder {
    /// (source, output), at most one per source.
    rules: Vec<(InputId, InputId)>,
    latch: Vec<(InputId, Latch)>,
    /// How many holds keep each delivered id down in the game: rebound outputs, and physical
    /// presses passed through (so a physical W and X → W share one press and one release).
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

    /// The rules to decide later presses with; held presses keep what they did. Of rules with
    /// the same source the first wins, and at most [`MAX_REBINDS`] are kept.
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

    /// No rules and nothing held: every event goes on as it is.
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

    /// Whether the source's press reached the game as another key (or did before focus loss):
    /// its own repeats, characters and release are not the game's.
    pub(crate) fn takes(&self, source: InputId) -> bool {
        matches!(
            self.latch_of(source),
            Some(Latch::Rebound(_) | Latch::Orphaned)
        )
    }

    /// One more hold on `id`; the new count.
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

    /// One hold less on `id`; the new count.
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

    /// The first say on an event of a source whose earlier press is latched, before the
    /// router's own handling (hotkeys, the UI) sees it. `None` leaves the event to the router;
    /// a stale latch is dropped then, for a press to be decided anew.
    ///
    /// `active`: the rules apply now (in game, the UI closed). `stateless`: the platform sends
    /// this key's auto-repeat as presses and no release on focus loss (GLFW's key -1).
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
                // A press with no release since the last: the release went missing, or this is
                // a stateless key's auto-repeat. Decided anew; the game keeps a rebound output
                // down until that output's next release.
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

    /// A repeat of a rebound source: the output's repeat while the rules apply and the output
    /// is a key (mouse buttons never repeat; a repeat would release them).
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

    /// One hold less on a rebound output: its release when it was the last.
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

    /// A press the router would forward: rebound when `active` and a rule has this source,
    /// else passed through. Either way the game gets a press only if the id was not already
    /// held down by another source.
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

    /// A release the router would forward, of a source not taken by [`latched`]
    /// (passed through, or pressed before anything was latched).
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
            // `latched` decides these; only reached if it was skipped.
            Some(Latch::Rebound(output)) => self.let_go(output),
            Some(Latch::Orphaned) => Delivery::Consume,
            // A press the router took (the menu was open, say) while a rule holds the id down:
            // the rule's own release lets go of it.
            None if self.rule_holds(source) => Delivery::Consume,
            None => Delivery::Forward,
        }
    }

    /// Whether a held rule has `id` down in the game.
    pub(crate) fn rule_holds(&self, id: InputId) -> bool {
        self.latch
            .iter()
            .any(|&(_, latch)| latch == Latch::Rebound(id))
    }

    /// Forgets a press the game got as it was, whose release never came, without giving the
    /// game anything: the platform found the key or button up, and the game let go of it by
    /// itself (26.x drops input events while it loads or saves a world, and resets its own
    /// key state when the screen changes). Returns whether `source` had such a press.
    pub(crate) fn forget_passthrough(&mut self, source: InputId) -> bool {
        if self.latch_of(source) != Some(Latch::Passthrough) {
            return false;
        }
        self.unlatch(source);
        self.unhold(source);
        true
    }

    /// The window lost focus: the releases the game must get now, for outputs nothing holds
    /// any more. `platform_releases(source)`: the platform still sends that source's release
    /// (GLFW sends none for key -1, SDL3 none for mouse buttons).
    ///
    /// Rebound sources whose release comes become orphaned (that release is then dropped);
    /// passed-through ones stay (their release goes to the game). Latches whose release never
    /// comes are dropped, and an id they alone held is released too.
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

    /// The source was let go although its release never came (the platform's check of the
    /// real key state): as its release would, with the event to give the game if any.
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

    /// As the router does when its own handling forwards everything.
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
        // Not in game: the button is the game's as it is.
        assert_eq!(event(&mut r, MOUSE4, Press, false), Forward);
        assert_eq!(event(&mut r, MOUSE4, Release, true), Forward);
        // Other keys pass through.
        assert_eq!(event(&mut r, W, Press, true), Forward);
        assert_eq!(event(&mut r, W, Repeat, true), Forward);
        assert_eq!(event(&mut r, W, Release, true), Forward);
        assert!(r.latch.is_empty() && r.held.is_empty());
    }

    #[test]
    fn the_release_follows_the_press_whatever_the_rules_became() {
        // Changed to another key.
        let mut r = rebinder(&[(A, F3)]);
        assert_eq!(event(&mut r, A, Press, true), send(F3, Press));
        r.set_rules(vec![(A, BACKSPACE)]);
        assert_eq!(event(&mut r, A, Repeat, true), send(F3, Repeat));
        assert_eq!(event(&mut r, A, Release, true), send(F3, Release));
        assert_eq!(event(&mut r, A, Press, true), send(BACKSPACE, Press));
        // Removed, and the rebinding turned off (no rules).
        r.set_rules(vec![]);
        assert_eq!(event(&mut r, A, Release, true), send(BACKSPACE, Release));
        assert_eq!(event(&mut r, A, Press, true), Forward);
        // Switched to a mouse button.
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
        // Pressed in a screen, released in game.
        let mut r = rebinder(&[(A, F3)]);
        assert_eq!(event(&mut r, A, Press, false), Forward);
        assert_eq!(event(&mut r, A, Repeat, true), Forward);
        assert_eq!(event(&mut r, A, Release, true), Forward);
        // Pressed before the rule existed.
        let mut r = rebinder(&[(B, C)]);
        assert_eq!(event(&mut r, A, Press, true), Forward);
        r.set_rules(vec![(A, F3)]);
        assert_eq!(event(&mut r, A, Release, true), Forward);
        // Pressed before anything was tracked.
        let mut r = rebinder(&[(A, F3)]);
        assert_eq!(event(&mut r, A, Release, true), Forward);
    }

    #[test]
    fn repeats_follow_the_output_only_while_active() {
        let mut r = rebinder(&[(X, W), (C, LEFT)]);
        assert_eq!(event(&mut r, X, Press, true), send(W, Press));
        assert_eq!(event(&mut r, X, Repeat, true), send(W, Repeat));
        // The UI or a screen opened while held: the output stays down, nothing repeats.
        assert_eq!(event(&mut r, X, Repeat, false), Consume);
        assert_eq!(event(&mut r, X, Release, false), send(W, Release));
        // A mouse button never repeats (a repeat would release it).
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
        // Physical W, then X → W: W goes up only when both are let go.
        let mut r = rebinder(&[(X, W)]);
        assert_eq!(event(&mut r, W, Press, true), Forward);
        assert_eq!(event(&mut r, X, Press, true), Consume);
        assert_eq!(event(&mut r, X, Release, true), Consume);
        assert_eq!(event(&mut r, W, Repeat, true), Forward);
        assert_eq!(event(&mut r, W, Release, true), Forward);
        // The other way round.
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
        // Both are taken sources and held outputs: held wins.
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
        // GLFW: every key and button gets its release after the focus callback.
        let mut releases = r.focus_lost(|_| true);
        releases.sort_by_key(|o| o.id);
        assert_eq!(releases, [output(C, Release), output(F3, Release)]);
        assert_eq!(r.state().taken.len(), 2, "orphaned sources stay up");
        assert!(r.state().held.is_empty());
        // The platform's own releases: the plain key's goes to the game.
        assert_eq!(event(&mut r, W, Release, true), Forward);
        assert_eq!(event(&mut r, X, Release, true), Consume);
        assert_eq!(event(&mut r, MOUSE4, Release, true), Consume);
        assert!(r.latch.is_empty() && r.held.is_empty());
    }

    #[test]
    fn focus_loss_without_rules_keeps_plain_releases() {
        // Rules exist but none is held: W must still be released by its own release.
        let mut r = rebinder(&[(X, F3)]);
        event(&mut r, W, Press, true);
        assert!(r.focus_lost(|_| true).is_empty());
        assert_eq!(event(&mut r, W, Release, true), Forward);
    }

    #[test]
    fn a_source_whose_release_never_comes_works_after_focus_loss() {
        // SDL3 releases no mouse button on focus loss.
        let sdl = |id: InputId| matches!(id, InputId::Key(_));
        let mut r = rebinder(&[(MOUSE4, F3)]);
        assert_eq!(event(&mut r, MOUSE4, Press, true), send(F3, Press));
        assert_eq!(event(&mut r, LEFT, Press, true), Forward);
        let mut releases = r.focus_lost(sdl);
        releases.sort_by_key(|o| o.id);
        // The left button the game got as it was is let go too: nothing will release it.
        assert_eq!(releases, [output(F3, Release), output(LEFT, Release)]);
        assert!(r.latch.is_empty() && r.held.is_empty());
        // Back in the game: the next press works.
        assert_eq!(event(&mut r, MOUSE4, Press, true), send(F3, Press));
        assert_eq!(event(&mut r, MOUSE4, Release, true), send(F3, Release));
    }

    #[test]
    fn an_orphaned_source_pressed_again_is_decided_anew() {
        // The orphan's release went missing (e.g. eaten by the IME).
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
        // X's release was lost; F3 stays down in the game until the next release fixes it.
        assert_eq!(event(&mut r, X, Press, true), send(F3, Press));
        assert_eq!(event(&mut r, X, Release, true), send(F3, Release));
        assert!(r.held.is_empty());
        // The same for a plain key.
        assert_eq!(event(&mut r, W, Press, true), Forward);
        assert_eq!(event(&mut r, W, Press, true), Forward);
        assert_eq!(event(&mut r, W, Release, true), Forward);
        assert!(r.held.is_empty());
    }

    #[test]
    fn a_stateless_key_repeats_with_presses() {
        // GLFW's key -1 (Japanese keys): auto-repeat comes as presses.
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
        // GLFW sends no release for it on focus loss: dropped, not orphaned.
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
        // The late release is not latched any more.
        assert_eq!(event(&mut r, X, Release, true), Forward);
        event(&mut r, W, Press, true);
        assert_eq!(r.release_source(W), Some(output(W, Release)));
    }

    #[test]
    fn a_release_of_an_id_a_rule_holds_is_the_rules() {
        // The router took W's press (the menu was open) while X → W held W down.
        let mut r = rebinder(&[(X, W)]);
        assert_eq!(event(&mut r, X, Press, true), send(W, Press));
        assert_eq!(r.release(W), Consume, "X still holds W");
        assert_eq!(event(&mut r, X, Release, true), send(W, Release));
        // Nothing holds it any more.
        assert_eq!(r.release(W), Forward);
        assert_eq!(r.release(X), Forward);
    }

    #[test]
    fn a_stale_passthrough_press_is_forgotten_without_a_release() {
        const H: InputId = InputId::Key(11);
        // 26.x dropped the release of the click that started loading a world.
        let mut r = rebinder(&[(H, LEFT)]);
        assert_eq!(event(&mut r, LEFT, Press, false), Forward);
        assert_eq!(r.state().passthrough, [LEFT]);
        assert!(r.state().held.is_empty());
        // Only presses of a rule's output are listed.
        assert_eq!(event(&mut r, W, Press, false), Forward);
        assert_eq!(r.state().passthrough, [LEFT]);
        assert!(r.forget_passthrough(LEFT));
        assert!(!r.forget_passthrough(LEFT), "only once");
        assert!(r.state().passthrough.is_empty());
        // The rule's press reaches the game again.
        assert_eq!(event(&mut r, H, Press, true), send(LEFT, Press));
        assert!(!r.forget_passthrough(H), "a rebound press is not forgotten");
        assert_eq!(event(&mut r, H, Release, true), send(LEFT, Release));
        // A late release of the forgotten press is the game's.
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
