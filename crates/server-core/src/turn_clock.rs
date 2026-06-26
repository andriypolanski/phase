//! Per-turn chess clock for online multiplayer (server-authoritative).
//!
//! When a host configures `timer_seconds` at room creation, the server
//! enforces a countdown for whichever human seat currently holds a sole
//! authorized submitter slot. Expiry applies `PassPriority` on that seat's
//! behalf when legal. This is a session/transport concern — not an engine
//! rule — analogous to `crate::takeback`.

use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::player::PlayerId;

use crate::protocol::ServerMessage;
use crate::session::{acting_players, GameSession, SessionManager};

/// Rolling turn-clock state for a live session.
#[derive(Debug, Clone, Default)]
pub struct TurnClockState {
    /// Human seat whose clock is currently running, if any.
    pub active_player: Option<PlayerId>,
    /// Wall-clock deadline (unix ms) for the active player's remaining budget.
    pub deadline_ms: Option<u64>,
    /// Cached remaining ms — updated on each sync for reconnect/bootstrap.
    pub remaining_ms: Option<u32>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Returns the human seat that should be clocked, if any.
///
/// The clock runs only when exactly one authorized submitter exists and that
/// seat is human. Simultaneous-decision states (mulligans, etc.) pause the
/// clock so nobody is timed out mid-vote.
pub fn clock_target_player(session: &GameSession) -> Option<PlayerId> {
    let budget_secs = session.timer_seconds.filter(|s| *s > 0)?;
    if budget_secs == 0 || !session.game_started {
        return None;
    }
    if session.pending_takeback.is_some() {
        return None;
    }
    if matches!(session.state.waiting_for, WaitingFor::GameOver { .. }) {
        return None;
    }

    let submitters = acting_players(&session.state);
    if submitters.len() != 1 {
        return None;
    }
    let player = submitters[0];
    if session.ai_seats.contains(&player) {
        return None;
    }
    Some(player)
}

impl TurnClockState {
    pub fn sync_for_target(
        &mut self,
        target: Option<PlayerId>,
        timer_seconds: Option<u32>,
        now: u64,
    ) {
        let Some(target) = target else {
            *self = TurnClockState::default();
            return;
        };

        let budget_ms = timer_seconds.unwrap_or(0).saturating_mul(1000);
        if budget_ms == 0 {
            *self = TurnClockState::default();
            return;
        }

        if self.active_player != Some(target) {
            self.active_player = Some(target);
            self.deadline_ms = Some(now.saturating_add(u64::from(budget_ms)));
            self.remaining_ms = Some(budget_ms);
            return;
        }

        if let Some(deadline) = self.deadline_ms {
            let remaining = deadline.saturating_sub(now).min(u64::from(u32::MAX)) as u32;
            self.remaining_ms = Some(remaining);
        }
    }

    pub fn sync_from_session(&mut self, session: &GameSession, now: u64) {
        self.sync_for_target(clock_target_player(session), session.timer_seconds, now);
    }

    pub fn is_expired(&self, now: u64) -> bool {
        self.active_player.is_some() && self.deadline_ms.is_some_and(|deadline| now >= deadline)
    }

    pub fn sync_message(&self) -> Option<ServerMessage> {
        let active_player = self.active_player?;
        let remaining_ms = self.remaining_ms?;
        Some(ServerMessage::TurnClockSync {
            active_player,
            remaining_ms,
            running: remaining_ms > 0,
        })
    }
}

impl GameSession {
    pub fn refresh_turn_clock(&mut self) {
        let now = now_ms();
        let target = clock_target_player(self);
        self.turn_clock
            .sync_for_target(target, self.timer_seconds, now);
    }

    pub fn turn_clock_sync_message(&self) -> Option<ServerMessage> {
        self.turn_clock.sync_message()
    }

    pub fn turn_clock_expired(&self) -> bool {
        self.turn_clock.is_expired(now_ms())
    }
}

impl SessionManager {
    /// Applies a priority pass on behalf of the timed-out human seat.
    pub fn handle_turn_clock_timeout(
        &mut self,
        game_code: &str,
    ) -> Result<Option<crate::session::ActionResult>, String> {
        let session = self
            .sessions
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found: {game_code}"))?;

        if !session.turn_clock_expired() {
            return Ok(None);
        }

        let player = session
            .turn_clock
            .active_player
            .ok_or_else(|| "Turn clock expired without an active player".to_string())?;
        let token = session.player_tokens[player.0 as usize].clone();
        if token.is_empty() {
            return Err("Timed-out seat has no player token".to_string());
        }

        let (legal_actions, _, _) = engine::ai_support::legal_actions_full(&session.state);
        if !legal_actions.contains(&GameAction::PassPriority) {
            // Cannot safely auto-act — reset the clock and wait for human input.
            session.refresh_turn_clock();
            return Ok(None);
        }

        drop(session);
        self.handle_action(game_code, &token, GameAction::PassPriority)
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::game_state::WaitingFor;
    use engine::types::match_config::MatchConfig;
    use std::collections::{HashMap, HashSet, VecDeque};

    use crate::session::GameSession;
    use engine::game::deck_loading::PlayerDeckPayload;
    use engine::types::format::FormatConfig;
    use engine::types::game_state::GameState;

    fn sample_session(timer_seconds: Option<u32>) -> GameSession {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        state.match_config = MatchConfig::default();
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.priority_player = PlayerId(0);
        state.active_player = PlayerId(0);
        GameSession {
            game_code: "TEST01".to_string(),
            state,
            player_tokens: vec!["tok0".into(), "tok1".into()],
            connected: vec![true, true],
            decks: vec![Some(PlayerDeckPayload::default()); 2],
            display_names: vec!["P0".into(), "P1".into()],
            reservations: HashMap::new(),
            timer_seconds,
            player_count: 2,
            ai_seats: HashSet::new(),
            ai_configs: HashMap::new(),
            ai_session: None,
            lobby_meta: None,
            game_started: true,
            start_when_full: true,
            ranked: false,
            start_events: Vec::new(),
            pending_takeback: None,
            takeback_history: VecDeque::new(),
            turn_clock: TurnClockState::default(),
        }
    }

    #[test]
    fn clock_targets_sole_human_submitter() {
        let session = sample_session(Some(60));
        assert_eq!(clock_target_player(&session), Some(PlayerId(0)));
    }

    #[test]
    fn clock_disabled_when_timer_unset() {
        let session = sample_session(None);
        assert!(clock_target_player(&session).is_none());
    }

    #[test]
    fn refresh_resets_budget_on_player_change() {
        let mut session = sample_session(Some(30));
        let mut clock = TurnClockState::default();
        let now = 1_000_000u64;
        clock.sync_for_target(Some(PlayerId(0)), Some(30), now);
        assert_eq!(clock.active_player, Some(PlayerId(0)));
        assert_eq!(clock.remaining_ms, Some(30_000));

        clock.sync_for_target(Some(PlayerId(1)), Some(30), now + 5_000);
        assert_eq!(clock.active_player, Some(PlayerId(1)));
        assert_eq!(clock.remaining_ms, Some(30_000));
    }

    #[test]
    fn sync_message_includes_running_flag() {
        let clock = TurnClockState {
            active_player: Some(PlayerId(0)),
            deadline_ms: Some(2_000),
            remaining_ms: Some(1_500),
        };
        match clock.sync_message() {
            Some(ServerMessage::TurnClockSync {
                active_player,
                remaining_ms,
                running,
            }) => {
                assert_eq!(active_player, PlayerId(0));
                assert_eq!(remaining_ms, 1_500);
                assert!(running);
            }
            other => panic!("expected TurnClockSync, got {other:?}"),
        }
    }
}
