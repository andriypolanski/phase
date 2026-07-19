//! Integration regression for GitHub issue #5977 — The Hunger Tide Rises,
//! chapter IV.
//!
//! Oracle: "Sacrifice any number of creatures. Search your library and/or
//! graveyard for a creature card with mana value less than or equal to the
//! number of creatures sacrificed this way and put it onto the battlefield.
//! If you search your library this way, shuffle."
//!
//! Reported bug: the search/pick step misbehaved — a bare-`and` split left a
//! stray `ChangeZone { target: ParentTarget }` sibling that could duplicate the
//! battlefield put during resolution.
//!
//! Parser regressions in `oracle_effect` assert the lowered AST shape; this
//! test drives the production `resolve_ability_chain` path: sacrifice choice →
//! multi-zone `SearchChoice` → single battlefield arrival for the picked card.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CHAPTER_IV: &str = "Sacrifice any number of creatures. Search your library and/or graveyard for a creature card with mana value less than or equal to the number of creatures sacrificed this way and put it onto the battlefield. If you search your library this way, shuffle.";

fn chapter_iv_ability() -> ResolvedAbility {
    let def = parse_effect_chain(CHAPTER_IV, AbilityKind::Spell);
    build_resolved_from_def(&def, ObjectId(100), P0)
}

fn add_library_creature(
    state: &mut GameState,
    card_id: u64,
    player: PlayerId,
    name: &str,
    mana_cost: ManaCost,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Library,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = mana_cost.clone();
    obj.base_mana_cost = mana_cost;
    id
}

fn battlefield_creature_count(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count()
}

fn drive_sacrifice_search_put(
    state: &mut GameState,
    chain: &ResolvedAbility,
    sacrifice: &[ObjectId],
    search_pick: ObjectId,
) {
    let mut events = Vec::new();
    resolve_ability_chain(state, chain, &mut events, 0).expect("chapter IV begins resolving");

    match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            count,
            min_count,
            up_to,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*min_count, 0, "any number includes zero (CR 107.1c)");
            assert!(*up_to, "Sacrifice any number uses variable selection");
            assert!(
                *count >= sacrifice.len(),
                "eligible sacrifice pool must cover chosen permanents"
            );
        }
        other => panic!("expected EffectZoneChoice for sacrifice, got {other:?}"),
    }

    let after_sacrifice = apply(
        state,
        P0,
        GameAction::SelectCards {
            cards: sacrifice.to_vec(),
        },
    )
    .expect("sacrifice selection accepted");

    assert_eq!(
        state.last_effect_count,
        Some(sacrifice.len() as i32),
        "sacrificed-this-way count must stamp for the CMC cap"
    );
    for id in sacrifice {
        assert!(
            state.players[0].graveyard.contains(id),
            "sacrificed creature {id:?} must be in graveyard"
        );
    }

    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        ..
    } = &after_sacrifice.waiting_for
    else {
        panic!(
            "expected SearchChoice after sacrifice, got {:?}",
            after_sacrifice.waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1, "chapter IV finds exactly one creature card");
    assert!(
        cards.contains(&search_pick),
        "picked card must be a legal search candidate, got {cards:?}"
    );

    apply(
        state,
        P0,
        GameAction::SelectCards {
            cards: vec![search_pick],
        },
    )
    .expect("search pick resolves the put-step continuation");

    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "chapter IV must finish after the search put (and shuffle when applicable), got {:?}",
        state.waiting_for
    );
}

#[test]
fn hunger_tide_chapter_iv_library_search_puts_creature_once() {
    let chain = chapter_iv_ability();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fodder_a = scenario
        .add_creature(P0, "Fodder A", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let fodder_b = scenario
        .add_creature(P0, "Fodder B", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut runner = scenario.build();
    let state = runner.state_mut();

    let finder = add_library_creature(state, 20, P0, "Library Finder", ManaCost::generic(2));
    let too_expensive = add_library_creature(state, 21, P0, "Too Expensive", ManaCost::generic(5));

    drive_sacrifice_search_put(state, &chain, &[fodder_a, fodder_b], finder);

    assert_eq!(
        state.objects[&finder].zone,
        Zone::Battlefield,
        "library pick must enter the battlefield"
    );
    assert_eq!(
        battlefield_creature_count(state),
        1,
        "only the searched creature may remain on the battlefield"
    );
    assert_eq!(
        state.objects[&too_expensive].zone,
        Zone::Library,
        "over-CMC library card must stay in the library"
    );
    assert!(
        !state.players[0].library.contains(&finder),
        "found card must leave the library"
    );
}

#[test]
fn hunger_tide_chapter_iv_graveyard_search_puts_creature_once() {
    let chain = chapter_iv_ability();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fodder = scenario
        .add_creature(P0, "Fodder", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let graveyard_finder = scenario
        .add_creature_to_graveyard(P0, "Graveyard Finder", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    let state = runner.state_mut();

    // A legal library candidate proves the search spans both zones but does not
    // duplicate the pick when the graveyard card is chosen.
    let _library_also = add_library_creature(state, 30, P0, "Library Also", ManaCost::generic(1));

    drive_sacrifice_search_put(state, &chain, &[fodder], graveyard_finder);

    assert_eq!(
        state.objects[&graveyard_finder].zone,
        Zone::Battlefield,
        "graveyard pick must enter the battlefield"
    );
    assert_eq!(
        battlefield_creature_count(state),
        1,
        "only the searched creature may remain on the battlefield"
    );
    assert!(
        state.players[0].graveyard.contains(&fodder),
        "sacrificed fodder stays in the graveyard"
    );
    assert!(
        !state.players[0].graveyard.contains(&graveyard_finder),
        "found graveyard card must leave the graveyard"
    );
}
