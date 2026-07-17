//! Issue #5820 — Susan Foreman: "If you would planeswalk, instead look at the
//! top two cards of your planar deck, put one on the bottom … and the other on
//! top, then planeswalk."

use engine::game::game_object::GameObject;
use engine::game::planechase::{
    active_plane, is_planar_ability_source, planar_ability_sentinel_id,
};
use engine::game::scenario::{GameRunner, P0};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition,
    ReplacementPlayerScope, ResolvedAbility,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// Planar object not placed in any zone vector — caller owns command vs deck placement
/// (mirrors `planechase_tests::create_planar_object`).
fn make_plane_object(state: &mut GameState, card_id: u32, name: &str) -> ObjectId {
    let id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let mut obj = GameObject::new(
        id,
        CardId(u64::from(card_id)),
        P0,
        name.to_string(),
        Zone::Command,
    );
    let mut card_type = CardType::default();
    card_type.core_types.push(CoreType::Plane);
    obj.card_types = card_type;
    state.objects.insert(id, obj);
    id
}

fn setup_planechase_two_deep(state: &mut GameState) -> (ObjectId, ObjectId, ObjectId) {
    state.format_config = FormatConfig::planechase();
    let active = make_plane_object(state, 1, "Active Plane");
    state.command_zone.push_back(active);
    let deck_top = make_plane_object(state, 2, "Deck Top");
    let deck_second = make_plane_object(state, 3, "Deck Second");
    if let Some(obj) = state.objects.get_mut(&deck_top) {
        obj.face_down = true;
    }
    if let Some(obj) = state.objects.get_mut(&deck_second) {
        obj.face_down = true;
    }
    state.planar_deck.push_back(deck_top);
    state.planar_deck.push_back(deck_second);
    state.planar_controller = Some(P0);
    (active, deck_top, deck_second)
}

fn install_susan_replacement(state: &mut GameState, source: ObjectId) {
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ArrangePlanarDeckTop {
            count: QuantityExpr::Fixed { value: 2 },
            keep_on_top: QuantityExpr::Fixed { value: 1 },
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Planeswalk,
    ));
    let mut replacement = ReplacementDefinition::new(ReplacementEvent::Planeswalk).execute(execute);
    replacement.valid_player = Some(ReplacementPlayerScope::You);
    state
        .objects
        .get_mut(&source)
        .expect("replacement host exists")
        .replacement_definitions
        .push(replacement);
}

#[test]
fn susan_foreman_planeswalk_arranges_planar_deck_then_rotates() {
    let mut state = GameState::new_two_player(42);
    state.active_player = P0;
    let (active_id, deck_top, deck_second) = setup_planechase_two_deep(&mut state);

    let susan = create_object(
        &mut state,
        CardId(100),
        P0,
        "Susan Foreman".to_string(),
        Zone::Battlefield,
    );
    install_susan_replacement(&mut state, susan);

    let sentinel = planar_ability_sentinel_id(P0);
    assert!(is_planar_ability_source(sentinel));
    let ability = ResolvedAbility::new(Effect::Planeswalk, vec![], sentinel, P0);
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0)
        .expect("planeswalking ability resolves");

    let WaitingFor::ArrangePlanarDeckTopChoice {
        player,
        cards,
        keep_on_top,
    } = state.waiting_for.clone()
    else {
        panic!(
            "Susan replacement must pause at planar deck arrange, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(cards, vec![deck_top, deck_second]);
    assert_eq!(keep_on_top, 1);
    assert_eq!(
        active_plane(&state),
        Some(active_id),
        "planeswalk must not happen before arrange completes"
    );
    assert!(
        state.pending_continuation.is_some(),
        "Planeswalk sub must be stashed while arrange pauses, got {:?}",
        state.pending_continuation
    );
    match &state.pending_continuation.as_ref().unwrap().chain.effect {
        Effect::Planeswalk => {}
        other => panic!("expected stashed Planeswalk continuation, got {other:?}"),
    }
    assert!(
        !is_planar_ability_source(state.pending_continuation.as_ref().unwrap().chain.source_id),
        "stashed planeswalk must not re-enter planar-die replacement"
    );

    let mut runner = GameRunner::from_state(state);
    runner
        .act(GameAction::SelectCards {
            cards: vec![deck_second],
        })
        .expect("arrange choice resolves");

    assert_eq!(
        active_plane(runner.state()),
        Some(deck_second),
        "planeswalk must complete after arrange"
    );
    assert!(
        runner.state().planar_deck.contains(&deck_top),
        "the bottomed card remains in the planar deck"
    );
}
