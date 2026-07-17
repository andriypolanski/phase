//! Open-bid life auction — Illicit Auction, Pain's Reward, Mages' Contest.
//!
//! Card-defined procedure (no CR keyword): the controller opens the bidding,
//! then in turn order each participant may top the standing high bid or pass.
//! When every other participant passes consecutively, the high bid stands; the
//! resolver publishes `last_life_auction` and drains chained sub-abilities
//! (`LoseLife` with `WinningBidAmount`, gain control, draw, etc.).

use std::collections::HashSet;

use crate::game::effects::{resolve_ability_chain, EffectError};
use crate::types::ability::{
    ControllerRef, Effect, EffectKind, LifeAuctionParticipants, LifeAuctionStartingBid,
    ResolvedAbility,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PayableResource, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 608.2c: Published when an auction completes; cleared at chain depth 0.
pub type LifeAuctionOutcome = (PlayerId, u32);

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::LifeAuction {
        participants,
        starting_bid,
        starting_with,
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "life_auction::resolve called with non-LifeAuction effect".into(),
        ));
    };

    let controller = ability.controller;
    let pool = build_participant_pool(state, ability, *participants, controller)?;
    if pool.is_empty() {
        emit_resolved(events, ability.source_id);
        return Ok(());
    }

    let opener = resolve_starting_player(state, ability.source_id, controller, starting_with);
    let ordered: Vec<PlayerId> = apnap_order_from(state, opener)
        .into_iter()
        .filter(|pid| pool.contains(pid))
        .collect();

    if matches!(starting_bid, LifeAuctionStartingBid::ControllerChooses) {
        state.pending_life_auction_participants = Some(ordered);
        state.waiting_for = WaitingFor::PayAmountChoice {
            player: controller,
            resource: PayableResource::Life,
            min: 0,
            max: max_biddable_life(state, controller),
            accumulated: 0,
            source_id: ability.source_id,
            pending_mana_ability: None,
        };
        return Ok(());
    }

    let opening = match starting_bid {
        LifeAuctionStartingBid::Fixed(value) => *value,
        LifeAuctionStartingBid::ControllerChooses => unreachable!(),
    };

    park_bid_prompt(
        state,
        ordered,
        opening,
        controller,
        ability.controller,
        ability.source_id,
        0,
        1,
    );
    Ok(())
}

/// Called from the PayAmountChoice handler once the controller announces Pain's
/// Reward opening bid (`last_effect_count`).
pub fn begin_after_opening_bid(state: &mut GameState, controller: PlayerId, source_id: ObjectId) {
    let Some(participants) = state.pending_life_auction_participants.take() else {
        return;
    };
    let opening = state.last_effect_count.unwrap_or(0).max(0) as u32;
    park_bid_prompt(
        state,
        participants,
        opening,
        controller,
        controller,
        source_id,
        0,
        1,
    );
}

pub fn record_pass(
    state: &mut GameState,
    round: LifeAuctionRoundState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    advance_after_pass_or_bid(
        state,
        events,
        LifeAuctionRoundState {
            passes_since_raise: round.passes_since_raise.saturating_add(1),
            ..round
        },
    )
}

pub fn record_bid(
    state: &mut GameState,
    round: LifeAuctionRoundState,
    bidder: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    advance_after_pass_or_bid(
        state,
        events,
        LifeAuctionRoundState {
            high_bid: amount,
            high_bidder: bidder,
            passes_since_raise: 0,
            round_index: round.round_index,
            ..round
        },
    )
}

pub fn advance_after_pass_or_bid(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    round: LifeAuctionRoundState,
) -> Result<(), EffectError> {
    let LifeAuctionRoundState {
        participants,
        round_index,
        high_bid,
        high_bidder,
        passes_since_raise,
        controller,
        source_id,
    } = round;

    let n = participants.len();
    if n == 0 {
        return Ok(());
    }

    if passes_since_raise >= n.saturating_sub(1) as u32 {
        return finalize_auction(state, high_bidder, high_bid, controller, source_id, events);
    }

    let next_index = (round_index + 1) % n;
    park_bid_prompt(
        state,
        participants,
        high_bid,
        high_bidder,
        controller,
        source_id,
        passes_since_raise,
        next_index,
    );
    Ok(())
}

pub fn finalize_auction(
    state: &mut GameState,
    winner: PlayerId,
    bid: u32,
    controller: PlayerId,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    state.last_life_auction = Some((winner, bid));
    emit_resolved(events, source_id);

    if let Some(cont) = state.pending_continuation.take() {
        // CR 608.2c: Drain payoff sub-abilities at depth 1 so the just-published
        // `last_life_auction` survives through `WinningBidAmount` / `WinningBidder`
        // resolution (mirrors vote ballot ledger at depth 1).
        resolve_ability_chain(state, &cont.chain, events, 1)?;
    }
    state.waiting_for = WaitingFor::Priority { player: controller };
    Ok(())
}

pub struct LifeAuctionRoundState {
    pub participants: Vec<PlayerId>,
    pub round_index: usize,
    pub high_bid: u32,
    pub high_bidder: PlayerId,
    pub passes_since_raise: u32,
    pub controller: PlayerId,
    pub source_id: ObjectId,
}

fn park_bid_prompt(
    state: &mut GameState,
    participants: Vec<PlayerId>,
    high_bid: u32,
    high_bidder: PlayerId,
    controller: PlayerId,
    source_id: ObjectId,
    passes_since_raise: u32,
    round_index: usize,
) {
    let round_index = round_index % participants.len().max(1);
    state.waiting_for = WaitingFor::LifeAuctionBid {
        player: participants[round_index],
        participants,
        round_index,
        high_bid,
        high_bidder,
        passes_since_raise,
        controller,
        source_id,
    };
}

fn emit_resolved(events: &mut Vec<GameEvent>, source_id: ObjectId) {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::LifeAuction,
        source_id,
        subject: None,
    });
}

fn build_participant_pool(
    state: &GameState,
    ability: &ResolvedAbility,
    participants: LifeAuctionParticipants,
    controller: PlayerId,
) -> Result<HashSet<PlayerId>, EffectError> {
    Ok(match participants {
        LifeAuctionParticipants::AllPlayers => state
            .seat_order
            .iter()
            .copied()
            .filter(|&pid| crate::game::players::is_alive(state, pid))
            .collect(),
        LifeAuctionParticipants::ControllerAndTargetController => {
            let spell_controller = crate::game::ability_utils::parent_target_controller(
                ability, state,
            )
            .ok_or(EffectError::InvalidParam(
                "LifeAuction ControllerAndTargetController requires spell target".into(),
            ))?;
            let mut set = HashSet::new();
            set.insert(controller);
            if crate::game::players::is_alive(state, spell_controller) {
                set.insert(spell_controller);
            }
            set
        }
    })
}

fn resolve_starting_player(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    starting_with: &ControllerRef,
) -> PlayerId {
    crate::game::filter::controller_ref_player(
        state,
        source_id,
        Some(controller),
        None,
        starting_with,
    )
    .unwrap_or(controller)
}

fn apnap_order_from(state: &GameState, start: PlayerId) -> Vec<PlayerId> {
    let seat_order = &state.seat_order;
    let n = seat_order.len();
    if n == 0 {
        return Vec::new();
    }
    let start_idx = seat_order.iter().position(|&id| id == start).unwrap_or(0);
    (0..n)
        .map(|offset| {
            crate::game::players::turn_order_index(start_idx, offset, n, state.turn_direction)
        })
        .map(|idx| seat_order[idx])
        .filter(|&player| crate::game::players::is_alive(state, player))
        .collect()
}

fn max_biddable_life(state: &GameState, player: PlayerId) -> u32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life.max(0) as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, LifeAuctionParticipants, LifeAuctionStartingBid};

    #[test]
    fn two_player_auction_passes_at_opening_bid() {
        let mut state = GameState::new_two_player(0);
        let ability = ResolvedAbility::new(
            Effect::LifeAuction {
                participants: LifeAuctionParticipants::AllPlayers,
                starting_bid: LifeAuctionStartingBid::Fixed(0),
                starting_with: ControllerRef::You,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let WaitingFor::LifeAuctionBid {
            high_bid,
            high_bidder,
            ..
        } = state.waiting_for
        else {
            panic!("expected LifeAuctionBid");
        };
        assert_eq!(high_bid, 0);
        assert_eq!(high_bidder, PlayerId(0));
    }
}
