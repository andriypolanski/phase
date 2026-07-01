//! Issue #3670 — Season of the Burrow modal modes resolve correctly.
//!
//! https://github.com/phase-rs/phase/issues/3670

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn fund_white(scenario: &mut GameScenario) {
    let mut pool = Vec::new();
    for _ in 0..2 {
        pool.push(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
    }
    for _ in 0..4 {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    scenario.with_mana_pool(P0, pool);
}

fn rabbit_token_count(state: &engine::types::game_state::GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("Rabbit"))
            })
        })
        .count()
}

#[test]
fn season_burrow_mode0_creates_rabbit_token() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario.add_real_card(P0, "Season of the Burrow", Zone::Hand, db);
    fund_white(&mut scenario);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(spell).modes(&[0]).resolve();

    assert_eq!(
        rabbit_token_count(outcome.state()),
        1,
        "the {{P}} mode must create a 1/1 white Rabbit token"
    );
}

#[test]
fn season_burrow_mode1_exiles_and_draws() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    for _ in 0..2 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let spell = scenario.add_real_card(P0, "Season of the Burrow", Zone::Hand, db);
    fund_white(&mut scenario);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner
        .cast(spell)
        .modes(&[1])
        .target_objects(&[bear])
        .resolve();

    outcome.assert_zone(&[bear], Zone::Exile);
    assert_eq!(
        outcome.hand_drawn(P1),
        1,
        "exiling the permanent must make its controller draw a card"
    );
    assert_eq!(
        outcome.hand_drawn(P0),
        0,
        "the caster does not draw from the exile mode"
    );
}

#[test]
fn season_burrow_mode2_returns_from_graveyard_with_indestructible() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grizzly = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let spell = scenario.add_real_card(P0, "Season of the Burrow", Zone::Hand, db);
    fund_white(&mut scenario);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner
        .cast(spell)
        .modes(&[2])
        .target_objects(&[grizzly])
        .resolve();

    outcome.assert_zone(&[grizzly], Zone::Battlefield);
    let obj = outcome
        .state()
        .objects
        .get(&grizzly)
        .expect("returned card");
    assert_eq!(
        obj.counters
            .get(&CounterType::Keyword(KeywordKind::Indestructible)),
        Some(&1),
        "the {{P}}{{P}}{{P}} mode must enter with an indestructible counter"
    );
}
