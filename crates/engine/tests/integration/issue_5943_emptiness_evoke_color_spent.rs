//! Issue #5943 — Evoking Emptiness doesn't fire its color-spent ETB.
//!
//! Emptiness Oracle:
//!   When this creature enters, if {W}{W} was spent to cast it, return target
//!   creature card with mana value 3 or less from your graveyard to the
//!   battlefield.
//!   When this creature enters, if {B}{B} was spent to cast it, put three -1/-1
//!   counters on up to one target creature.
//!   Evoke {W/B}{W/B}
//!
//! Classifier: supported_aspect_defect. The AST already carries
//! `TriggerCondition::ManaColorSpent { minimum: 2 }` on both ETBs; the defect
//! was runtime — `clear_post_collection_transients` wiped `colors_spent_to_cast`
//! after collection, so CR 603.4 ExactLive re-checks failed at resolution
//! (Adamant / color-spent class, not Evoke-specific). The tally must also
//! clear on battlefield exit (CR 400.7) so a blinked permanent does not
//! inherit its previous cast colors on re-entry.
//!
//! https://github.com/phase-rs/phase/issues/5943

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::actions::AlternativeCastDecision;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const EMPTINESS_ORACLE: &str = "When this creature enters, if {W}{W} was spent to cast it, \
return target creature card with mana value 3 or less from your graveyard to the battlefield.\n\
When this creature enters, if {B}{B} was spent to cast it, put three -1/-1 counters on up to one \
target creature.\n\
Evoke {W/B}{W/B}";

fn add_mana(state: &mut engine::types::game_state::GameState, color: ManaType, n: u32) {
    for _ in 0..n {
        state.players[0]
            .mana_pool
            .add(ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn emptiness_in_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand(P0, "Emptiness", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlack, ManaCostShard::WhiteBlack],
            generic: 4,
        })
        .from_oracle_text_with_keywords(&["Evoke"], EMPTINESS_ORACLE)
        .id()
}

/// CR 702.74a + CR 207.2c + CR 601.2h + CR 603.4: Evoking Emptiness for {W}{W}
/// must fire the white-spent ETB (return MV≤3 creature from GY) and then the
/// evoke sacrifice — the color tally must survive post-collection clearing.
#[test]
fn emptiness_evoke_ww_returns_graveyard_creature_then_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emptiness = emptiness_in_hand(&mut scenario);
    let gy_creature = scenario
        .add_creature_to_graveyard(P0, "Recoverable Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    // Enough for printed and evoke so the AlternativeCastChoice surfaces;
    // choosing Evoke spends only {W/B}{W/B} as white.
    add_mana(runner.state_mut(), ManaType::White, 6);

    let outcome = runner
        .cast(emptiness)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .target_objects(&[gy_creature])
        .resolve();

    outcome.assert_zone(&[gy_creature], Zone::Battlefield);
    outcome.assert_zone(&[emptiness], Zone::Graveyard);
}

/// CR 702.74a + CR 207.2c + CR 601.2h + CR 603.4: Evoking for {B}{B} must fire
/// the black-spent ETB (three -1/-1 counters) before the evoke sacrifice.
#[test]
fn emptiness_evoke_bb_puts_minus_counters_then_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emptiness = emptiness_in_hand(&mut scenario);
    let victim = scenario.add_creature(P1, "Victim Bear", 4, 4).id();

    let mut runner = scenario.build();
    add_mana(runner.state_mut(), ManaType::Black, 6);

    let outcome = runner
        .cast(emptiness)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .target_objects(&[victim])
        .resolve();

    outcome.assert_counters(victim, CounterType::Minus1Minus1, 3);
    outcome.assert_zone(&[emptiness], Zone::Graveyard);
}

/// CR 400.7 + CR 603.4: A permanent cast with {W}{W} must satisfy
/// `ManaColorSpent` on its **original** ETB, but after leaving and re-entering
/// without a new cast the stale color tally must not survive — the white-spent
/// branch must not fire again.
#[test]
fn emptiness_blinked_reentry_does_not_reuse_cast_color_tally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emptiness = emptiness_in_hand(&mut scenario);
    let first_return = scenario
        .add_creature_to_graveyard(P0, "First Return", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let blink_bait = scenario
        .add_creature_to_graveyard(P0, "Blink Bait", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();
    // Hard-cast for {W}{W}{4} (not Evoke) so Emptiness remains on the battlefield.
    add_mana(runner.state_mut(), ManaType::White, 2);
    add_mana(runner.state_mut(), ManaType::Colorless, 4);

    let outcome = runner
        .cast(emptiness)
        .alternative_cast(AlternativeCastDecision::Normal)
        .target_objects(&[first_return])
        .resolve();

    outcome.assert_zone(&[first_return], Zone::Battlefield);
    outcome.assert_zone(&[emptiness], Zone::Battlefield);
    assert!(
        runner.state().objects[&emptiness]
            .colors_spent_to_cast
            .get(engine::types::mana::ManaColor::White)
            >= 2,
        "precondition: the original cast must record two white mana before the blink"
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), emptiness, Zone::Exile, &mut events);
    assert_eq!(
        runner.state().objects[&emptiness]
            .colors_spent_to_cast
            .get(engine::types::mana::ManaColor::White),
        0,
        "colors_spent_to_cast must clear on battlefield exit"
    );

    engine::game::zones::move_to_zone(
        runner.state_mut(),
        emptiness,
        Zone::Battlefield,
        &mut events,
    );
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&blink_bait].zone,
        Zone::Graveyard,
        "after blink, the white-spent ETB must not fire — blink bait stays in the graveyard"
    );
    assert_eq!(
        runner.state().objects[&emptiness].zone,
        Zone::Battlefield,
        "Emptiness must remain on the battlefield after the blink"
    );
}
