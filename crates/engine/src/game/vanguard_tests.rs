//! Tests for the Vanguard runtime (CR 902). Declared from `game/mod.rs` so
//! `vanguard.rs` stays implementation-only.

use super::vanguard::{
    apply_vanguard_starting_life_modifiers, create_vanguard_from_card_face, is_vanguard_object,
    opening_hand_size, vanguard_for_player,
};
use crate::database::synthesis::synthesize_vanguard;
use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::format::FormatConfig;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::statics::{HandSizeModification, StaticMode};
use crate::types::zones::Zone;
use crate::types::StaticDefinition;

fn synthesized_vanguard_face(hand_modifier: i32, life_modifier: i32) -> CardFace {
    let mut face = CardFace::default();
    face.name = "Test Vanguard".to_string();
    face.card_type.core_types.push(CoreType::Vanguard);
    face.hand_modifier = hand_modifier;
    face.life_modifier = life_modifier;
    synthesize_vanguard(&mut face);
    face
}

fn vanguard_state() -> GameState {
    let mut state = GameState::new(FormatConfig::vanguard(), 2, 0);
    state.seat_order = vec![PlayerId(0), PlayerId(1)];
    state
}

#[test]
fn synthesize_vanguard_stamps_command_zone_on_statics() {
    let face = synthesized_vanguard_face(0, 0);
    assert!(
        face.static_abilities.is_empty(),
        "zero hand modifier should not synthesize a hand-size static"
    );
    assert!(face.triggers.is_empty());
}

#[test]
fn synthesize_vanguard_adds_hand_modifier_static() {
    let face = synthesized_vanguard_face(2, 0);
    assert_eq!(face.static_abilities.len(), 1);
    let static_def = &face.static_abilities[0];
    assert!(static_def.active_zones.contains(&Zone::Command));
    assert_eq!(
        static_def.mode,
        StaticMode::MaximumHandSize {
            modification: HandSizeModification::AdjustedBy(2),
        }
    );
    assert_eq!(
        static_def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
}

#[test]
fn create_vanguard_places_face_up_command_zone_object() {
    let mut state = vanguard_state();
    let face = synthesized_vanguard_face(0, 3);
    let id = create_vanguard_from_card_face(&mut state, &face, PlayerId(0));

    assert!(state.command_zone.contains(&id));
    assert!(is_vanguard_object(&state, id));
    assert_eq!(vanguard_for_player(&state, PlayerId(0)), Some(id));
    let obj = state.objects.get(&id).expect("vanguard object");
    assert!(!obj.face_down);
    assert_eq!(obj.life_modifier, 3);
}

#[test]
fn apply_vanguard_starting_life_modifiers_applies_delta() {
    let mut state = vanguard_state();
    let face = synthesized_vanguard_face(0, -4);
    create_vanguard_from_card_face(&mut state, &face, PlayerId(0));

    apply_vanguard_starting_life_modifiers(&mut state);

    assert_eq!(state.players[0].life, 16);
    assert_eq!(state.players[1].life, 20);
}

#[test]
fn opening_hand_size_uses_vanguard_hand_modifier() {
    let mut state = vanguard_state();
    let face = synthesized_vanguard_face(1, 0);
    create_vanguard_from_card_face(&mut state, &face, PlayerId(0));

    assert_eq!(opening_hand_size(&state, PlayerId(0)), 8);
    assert_eq!(opening_hand_size(&state, PlayerId(1)), 7);
}

#[test]
fn opening_hand_size_does_not_use_maximum_hand_size_layer() {
    let mut state = vanguard_state();
    let face = synthesized_vanguard_face(1, 0);
    let id = create_vanguard_from_card_face(&mut state, &face, PlayerId(0));

    // A printed maximum-hand-size static must not affect CR 902.5 opening draw.
    state
        .objects
        .get_mut(&id)
        .expect("vanguard object")
        .static_definitions
        .push(
            StaticDefinition::new(StaticMode::MaximumHandSize {
                modification: HandSizeModification::AdjustedBy(5),
            })
            .affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );

    assert_eq!(opening_hand_size(&state, PlayerId(0)), 8);
}

#[test]
fn is_vanguard_object_rejects_non_vanguard_command_zone_cards() {
    let mut state = vanguard_state();
    let id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let mut obj = crate::game::game_object::GameObject::new(
        id,
        crate::types::identifiers::CardId(id.0),
        PlayerId(0),
        "Not Vanguard".to_string(),
        Zone::Command,
    );
    obj.controller = PlayerId(0);
    state.objects.insert(id, obj);
    state.command_zone.push_back(id);

    assert!(!is_vanguard_object(&state, id));
}
