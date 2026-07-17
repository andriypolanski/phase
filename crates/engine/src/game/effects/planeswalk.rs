//! CR 701.31 / CR 901.8: Resolver for `Effect::Planeswalk`.
//!
//! CR 901.8 / CR 901.9c: when a player rolls the Planeswalker symbol on the
//! planar die, the planeswalking ability triggers and is put on the stack
//! (see `planechase::roll_planar_die`). On resolution, its controller — the
//! roller, CR 901.8 — planeswalks (CR 701.31).
//!
//! Every `Effect::Planeswalk` resolution routes through
//! `planechase::planeswalk_through_replacements` so "if you would planeswalk"
//! replacements (Susan Foreman) apply regardless of source. Encounter / SBA /
//! leave-game planeswalks (CR 701.31c) call `planechase::planeswalk` directly.

use crate::game::planechase::{self, PlaneswalkCause};
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 901.8 / CR 901.9c / CR 701.31: resolve a planeswalk effect — route through
/// the replacement pipeline, then perform the zone rotation on `Execute`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let cause = if planechase::is_planar_ability_source(ability.source_id) {
        PlaneswalkCause::PlanarDie
    } else {
        PlaneswalkCause::Instruction
    };
    match planechase::planeswalk_through_replacements(
        state,
        ability.controller,
        cause,
        ability.replacement_applied.clone(),
        events,
    ) {
        crate::game::replacement::ReplacementResult::Execute(_) => Ok(()),
        crate::game::replacement::ReplacementResult::Prevented => Ok(()),
        crate::game::replacement::ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            Ok(())
        }
    }
}
