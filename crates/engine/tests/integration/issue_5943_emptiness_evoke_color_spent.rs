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
//! was runtime lifecycle of `colors_spent_to_cast`:
//!
//!   * Too short: `clear_post_collection_transients` wiped the tally before CR
//!     603.4 resolution re-checks on the original ETB.
//!   * Too long: the tally survived battlefield exit or stack→graveyard without
//!     becoming the permanent, so blink/reanimate paths inherited stale colors.
//!
//! https://github.com/phase-rs/phase/issues/5943

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const EMPTINESS_ORACLE: &str = "When this creature enters, if {W}{W} was spent to cast it, \
return target creature card with mana value 3 or less from your graveyard to the battlefield.\n\
When this creature enters, if {B}{B} was spent to cast it, put three -1/-1 counters on up to one \
target creature.\n\
Evoke {W/B}{W/B}";

const BLINK: &str =
    "Exile target creature you control. Return it to the battlefield under its owner's control.";

const COUNTERSPELL: &str = "Counter target spell.";

const REANIMATE: &str = "Return target creature card from your graveyard to the battlefield.";

fn add_mana(
    state: &mut engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
    color: ManaType,
    n: u32,
) {
    for _ in 0..n {
        state.players[player.0 as usize]
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
    add_mana(runner.state_mut(), P0, ManaType::White, 6);

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
    add_mana(runner.state_mut(), P0, ManaType::Black, 6);

    let outcome = runner
        .cast(emptiness)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .target_objects(&[victim])
        .resolve();

    outcome.assert_counters(victim, CounterType::Minus1Minus1, 3);
    outcome.assert_zone(&[emptiness], Zone::Graveyard);
}

/// CR 400.7 + CR 603.4: Blink through the production cast/zone-change pipeline
/// must not let a hard-cast Emptiness reuse its prior {W}{W} tally on re-entry.
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
    let blink = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift Test", true, BLINK)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    add_mana(runner.state_mut(), P0, ManaType::White, 2);
    add_mana(runner.state_mut(), P0, ManaType::Colorless, 4);

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
            .get(ManaColor::White)
            >= 2,
        "precondition: the original cast must record two white mana before the blink"
    );

    runner.cast(blink).target_objects(&[emptiness]).resolve();

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

/// CR 400.7: A {W}{W} Emptiness countered to the graveyard must drop its cast
/// colors; a later Reanimate must not satisfy `ManaColorSpent` on entry.
#[test]
fn emptiness_countered_then_reanimated_does_not_reuse_cast_color_tally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let emptiness = emptiness_in_hand(&mut scenario);
    let reanimate_bait = scenario
        .add_creature_to_graveyard(P0, "Reanimate Bait", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let counterspell = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id();
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);

    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    add_mana(runner.state_mut(), P0, ManaType::White, 2);
    add_mana(runner.state_mut(), P0, ManaType::Colorless, 4);

    runner
        .cast(emptiness)
        .alternative_cast(AlternativeCastDecision::Normal)
        .commit();

    assert_eq!(
        runner.state().objects[&emptiness].zone,
        Zone::Stack,
        "Emptiness must be on the stack before Counterspell"
    );

    // CR 117.7: P0 passes so P1 may cast Counterspell in response.
    runner.act(GameAction::PassPriority).expect("P0 pass");

    add_mana(runner.state_mut(), P1, ManaType::Blue, 2);
    runner
        .cast(counterspell)
        .target_objects(&[emptiness])
        .resolve();

    assert_eq!(
        runner.state().objects[&emptiness].zone,
        Zone::Graveyard,
        "Counterspell must move Emptiness to the graveyard"
    );
    assert_eq!(
        runner.state().objects[&emptiness]
            .colors_spent_to_cast
            .get(ManaColor::White),
        0,
        "cast colors must clear when the spell leaves the stack without resolving"
    );

    runner
        .cast(reanimate)
        .target_objects(&[emptiness])
        .resolve();

    assert_eq!(
        runner.state().objects[&emptiness].zone,
        Zone::Battlefield,
        "Reanimate must return Emptiness to the battlefield"
    );
    assert_eq!(
        runner.state().objects[&reanimate_bait].zone,
        Zone::Graveyard,
        "without cast colors, the white-spent ETB must not return reanimate bait"
    );
}
