//! CR 902: Vanguard variant regression tests (#5056).

use engine::database::synthesis::synthesize_vanguard;
use engine::game::deck_loading::{load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload};
use engine::game::vanguard::{opening_hand_size, vanguard_for_player};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

fn vanguard_face(hand: i32, life: i32) -> CardFace {
    let mut face = CardFace::default();
    face.name = "Test Vanguard Avatar".to_string();
    face.card_type.core_types.push(CoreType::Vanguard);
    face.hand_modifier = hand;
    face.life_modifier = life;
    synthesize_vanguard(&mut face);
    face
}

#[test]
fn vanguard_load_applies_life_modifier_and_command_zone_object() {
    let mut state = GameState::new(FormatConfig::vanguard(), 2, 1);
    state.seat_order = vec![PlayerId(0), PlayerId(1)];

    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: vec![DeckEntry {
                card: {
                    let mut face = CardFace::default();
                    face.name = "Forest".to_string();
                    face.card_type.core_types.push(CoreType::Land);
                    face
                },
                count: 60,
            }],
            vanguard: vec![DeckEntry {
                card: vanguard_face(0, 5),
                count: 1,
            }],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: vec![DeckEntry {
                card: {
                    let mut face = CardFace::default();
                    face.name = "Island".to_string();
                    face.card_type.core_types.push(CoreType::Land);
                    face
                },
                count: 60,
            }],
            vanguard: vec![DeckEntry {
                card: vanguard_face(-1, 0),
                count: 1,
            }],
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    assert_eq!(state.players[0].life, 25);
    assert_eq!(state.players[1].life, 20);
    assert!(vanguard_for_player(&state, PlayerId(0)).is_some());
    assert!(vanguard_for_player(&state, PlayerId(1)).is_some());
    assert_eq!(opening_hand_size(&state, PlayerId(0)), 7);
    assert_eq!(opening_hand_size(&state, PlayerId(1)), 6);
}
