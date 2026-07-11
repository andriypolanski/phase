//! CR 902: Vanguard casual variant runtime.
//!
//! Each player has one face-up vanguard card in the command zone for the whole
//! game. Starting life and hand size are modified per CR 902.4/902.5; abilities
//! function from the command zone per CR 902.7.

use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::zones::create_object;
use crate::types::card::CardFace;
use crate::types::format::GameFormat;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 902.6: Whether `id` is a player's vanguard card in the command zone.
pub fn is_vanguard_object(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|obj| obj.is_vanguard)
}

/// CR 902.6: The vanguard object controlled by `player`, if any.
pub fn vanguard_for_player(state: &GameState, player: PlayerId) -> Option<ObjectId> {
    state.command_zone.iter().copied().find(|&id| {
        state
            .objects
            .get(&id)
            .is_some_and(|obj| obj.is_vanguard && obj.controller == player)
    })
}

/// CR 902.6: Create a vanguard `GameObject` face up in the command zone.
pub fn create_vanguard_from_card_face(
    state: &mut GameState,
    card_face: &CardFace,
    owner: PlayerId,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let obj_id = create_object(state, card_id, owner, card_face.name.clone(), Zone::Command);
    let obj = state.objects.get_mut(&obj_id).expect("just created");
    apply_card_face_to_object(obj, card_face);
    obj.is_vanguard = true;
    obj.face_down = false;
    obj_id
}

/// CR 902.4: Apply each player's vanguard life modifier after decks load.
pub fn apply_vanguard_starting_life_modifiers(state: &mut GameState) {
    if state.format_config.format != GameFormat::Vanguard {
        return;
    }
    for player in state.seat_order.clone() {
        let Some(vanguard_id) = vanguard_for_player(state, player) else {
            continue;
        };
        let modifier = state
            .objects
            .get(&vanguard_id)
            .map(|obj| obj.life_modifier)
            .unwrap_or(0);
        if modifier == 0 {
            continue;
        }
        if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
            p.life += modifier;
        }
    }
}

/// CR 902.5: Opening-hand draw size for `player` (7 ± vanguard hand modifier).
pub fn opening_hand_size(state: &GameState, player: PlayerId) -> usize {
    if state.format_config.format != GameFormat::Vanguard {
        return 7;
    }
    super::turns::maximum_hand_size_for_player(state, player)
}
