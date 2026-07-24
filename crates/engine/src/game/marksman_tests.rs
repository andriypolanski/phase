//! Runtime + parser tests for the repeated-optional-payment + reflexive-modal
//! driver (Hawkeye, Master Marksman — "Trick Arrows"). CR 603.12a / CR 700.2d.
//!
//! These drive the real production seams:
//! - the parser (`parse_oracle_text`) for the modal AST (B4);
//! - the production trigger-resolution function (`resolve_ability_chain` at
//!   depth 0, exactly as the engine resolves a stack trigger) to reach the
//!   batched `WaitingFor::AbilityModeChoice`;
//! - the real action handler (`apply`) for `SelectModes`, so the changed
//!   `WaitingFor`/`GameAction` route is exercised.

#![cfg(test)]

use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::resolve_ability_chain;
use crate::game::scenario::GameScenario;
use crate::parser::oracle::{parse_oracle_text, ParsedAbilities};
use crate::types::ability::{
    AbilityCondition, Effect, ModalChoice, QuantityExpr, QuantityRef, TargetRef,
};
use crate::types::actions::GameAction;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::mana::{ManaType, ManaUnit};
use crate::types::player::PlayerId;
use crate::types::resolution::ResolutionStateWire;
use crate::types::triggers::TriggerMode;

const HAWKEYE_ORACLE: &str = "First strike, reach\n\
     Trick Arrows — Whenever Hawkeye becomes tapped, you may pay {1} up to three times. \
     When you do, choose up to that many.\n\
     • Net — Target creature can't block this turn.\n\
     • Explosive — Hawkeye deals 2 damage to target player.\n\
     • Boomerang — Discard a card, then draw a card.";

const FRILLBACK_ORACLE: &str = "When this creature enters, you may pay {G} up to three times. \
     When you pay this cost one or more times, choose up to that many —\n\
     • Destroy target artifact or enchantment.\n\
     • Exile target player's graveyard.\n\
     • You gain 4 life.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn parse_hawkeye() -> ParsedAbilities {
    parse_oracle_text(
        HAWKEYE_ORACLE,
        "Hawkeye, Master Marksman",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &[],
    )
}

/// The reflexive sub-ability's modal (`when you do, choose up to that many`).
fn hawkeye_reflexive_modal(parsed: &ParsedAbilities) -> ModalChoice {
    let taps = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Taps)
        .expect("Hawkeye has a Taps trigger");
    let execute = taps.execute.as_ref().expect("Taps trigger has an execute");
    let reflexive = execute
        .sub_ability
        .as_ref()
        .expect("PayCost has a reflexive sub-ability");
    reflexive
        .modal
        .clone()
        .expect("reflexive sub carries a modal")
}

// ── B4 parser: the modal carries the resolution-local dynamic cap ────────

/// Revert discriminator: dropping the `"choose up to that many"` parser arm (or
/// the `ModalCountSpec::Dynamic { qty }` wiring) leaves the modal at the fixed
/// `(1, 1)` default with `dynamic_max_choices == None`, failing every assertion.
#[test]
fn hawkeye_modal_parses_with_resolution_local_dynamic_cap() {
    let parsed = parse_hawkeye();
    let modal = hawkeye_reflexive_modal(&parsed);

    assert_eq!(modal.min_choices, 0, "min is 0 — may choose no modes");
    assert_eq!(modal.mode_count, 3, "Net / Explosive / Boomerang");
    assert_eq!(
        modal.dynamic_max_choices,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::TimesCostPaidThisResolution
        }),
        "the cap binds to the resolution-local repeated-payment count"
    );
}

#[test]
fn tranquil_frillback_parses_as_repeated_green_payment_modal() {
    let parsed = parse_oracle_text(
        FRILLBACK_ORACLE,
        "Tranquil Frillback",
        &[],
        &["Creature".to_string()],
        &["Dinosaur".to_string()],
    );
    let execute = parsed.triggers[0].execute.as_deref().expect("ETB execute");
    assert!(matches!(execute.effect.as_ref(), Effect::PayCost { .. }));
    assert!(execute.optional);
    assert_eq!(execute.repeat_for, Some(QuantityExpr::Fixed { value: 3 }));
    let modal = execute.sub_ability.as_deref().expect("reflexive modal");
    assert_eq!(modal.condition, Some(AbilityCondition::WhenYouDo));
    assert_eq!(
        modal
            .modal
            .as_ref()
            .and_then(|modal| modal.dynamic_max_choices.clone()),
        Some(QuantityExpr::Ref {
            qty: QuantityRef::TimesCostPaidThisResolution
        })
    );
}

#[test]
fn tranquil_frillback_paying_once_and_choosing_life_gains_four() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 20);
    let frillback = scenario
        .add_creature_from_oracle(P0, "Tranquil Frillback", 3, 3, FRILLBACK_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Green,
            crate::types::identifiers::ObjectId(9_999),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    let parsed = parse_oracle_text(
        FRILLBACK_ORACLE,
        "Tranquil Frillback",
        &[],
        &["Creature".to_string()],
        &["Dinosaur".to_string()],
    );
    let execute = parsed.triggers[0].execute.as_deref().expect("ETB execute");
    let resolved = build_resolved_from_def(execute, frillback, P0);
    resolve_ability_chain(runner.state_mut(), &resolved, &mut Vec::new(), 0)
        .expect("resolve Frillback ETB");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::AbilityModeChoice { .. }
    ));
    runner
        .act(GameAction::SelectModes { indices: vec![2] })
        .expect("select Frillback life mode");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].life, 24);
}

// ── Runtime harness ─────────────────────────────────────────────────────

/// Hawkeye on P0's battlefield with `mana` colorless mana in pool, the Taps
/// trigger resolved through the production resolver (depth 0). Leaves the engine
/// at a batched `WaitingFor::AbilityModeChoice`.
fn hawkeye_runtime(mana: usize, p0_hand: &[&str]) -> crate::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.with_life(P1, 20);
    if !p0_hand.is_empty() {
        scenario.with_cards_in_hand(P0, p0_hand);
    }
    let hawkeye = scenario
        .add_creature_from_oracle(P0, "Hawkeye, Master Marksman", 3, 3, HAWKEYE_ORACLE)
        .id();
    if mana > 0 {
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(
                    ManaType::Colorless,
                    crate::types::identifiers::ObjectId(9_999),
                    false,
                    vec![],
                );
                mana
            ],
        );
    }
    let mut runner = scenario.build();

    let parsed = parse_hawkeye();
    let taps = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Taps)
        .unwrap();
    let execute = taps.execute.as_ref().unwrap();
    let resolved = build_resolved_from_def(execute, hawkeye, P0);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("trigger resolution");
    let _ = hawkeye;
    runner
}

fn k_count(state: &GameState) -> u32 {
    state
        .active_repeated_optional_payment_frame()
        .map_or(0, |frame| frame.optional_cost_payments_this_resolution)
}

fn p0_pool_total(state: &GameState) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0)
}

fn repeated_payment_driver_is_pending(state: &GameState) -> bool {
    state
        .active_repeated_optional_payment_frame()
        .is_some_and(|frame| frame.pending.is_some())
}

fn modal_cap(state: &GameState) -> Option<(usize, usize)> {
    match &state.waiting_for {
        WaitingFor::AbilityModeChoice { modal, .. } => Some((modal.min_choices, modal.max_choices)),
        _ => None,
    }
}

fn mode_cost_count(state: &GameState) -> Option<usize> {
    match &state.waiting_for {
        WaitingFor::AbilityModeChoice { modal, .. } => Some(modal.mode_costs.len()),
        _ => None,
    }
}

// ── Batched payment + modal cap ───────────────────────────────────────────

#[test]
fn batched_modal_offered_once_with_static_cap_from_affordability() {
    let runner = hawkeye_runtime(3, &[]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "batched path emits one AbilityModeChoice, not sequential OptionalEffectChoice"
    );
    assert_eq!(modal_cap(runner.state()), Some((0, 3)));
    assert_eq!(mode_cost_count(runner.state()), Some(3));
    assert!(
        repeated_payment_driver_is_pending(runner.state()),
        "payment settles on SelectModes, not before the modal"
    );
}

#[test]
fn afford_zero_caps_modal_at_zero_and_accepts_empty_decline() {
    let mut runner = hawkeye_runtime(0, &[]);
    assert_eq!(modal_cap(runner.state()), Some((0, 0)));
    runner
        .act(GameAction::SelectModes { indices: vec![] })
        .expect("decline with empty selection");
    assert_eq!(k_count(runner.state()), 0);
    assert!(
        runner
            .state()
            .active_repeated_optional_payment_frame()
            .is_none(),
        "decline clears the payment frame"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn afford_two_of_three_caps_modal_at_two() {
    let runner = hawkeye_runtime(2, &[]);
    assert_eq!(modal_cap(runner.state()), Some((0, 2)));
}

#[test]
fn select_three_modes_pays_three_mana() {
    let mut runner = hawkeye_runtime(3, &[]);
    runner
        .act(GameAction::SelectModes {
            indices: vec![0, 1, 2],
        })
        .expect("select three modes");
    assert_eq!(
        p0_pool_total(runner.state()),
        0,
        "three {{1}} payments were made in one step"
    );
    assert!(
        runner
            .state()
            .active_repeated_optional_payment_frame()
            .is_none(),
        "payment frame settles after batched mode resolution"
    );
}

#[test]
fn select_one_mode_pays_one_mana() {
    let mut runner = hawkeye_runtime(3, &[]);
    assert_eq!(p0_pool_total(runner.state()), 3);
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("select Explosive only");
    assert_eq!(
        p0_pool_total(runner.state()),
        2,
        "issue #6175: pay only for selected modes (1 of 3)"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Explosive still needs a target after the single {{1}} payment"
    );
}

#[test]
fn unaffordable_submit_after_pool_drain_keeps_batched_prompt() {
    let mut runner = hawkeye_runtime(3, &[]);
    for player in runner.state_mut().players.iter_mut() {
        if player.id == P0 {
            player.mana_pool.mana.clear();
        }
    }
    let err = runner.act(GameAction::SelectModes { indices: vec![0] });
    assert!(
        err.is_err(),
        "stale SelectModes must fail when the pool can no longer pay"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "prompt remains live after rejected submit"
    );
    assert!(
        repeated_payment_driver_is_pending(runner.state()),
        "batched pending must survive failed payment / affordability reject"
    );
}

#[test]
fn decline_empty_selection_skips_reflexive_at_k_zero() {
    let mut runner = hawkeye_runtime(3, &[]);
    runner
        .act(GameAction::SelectModes { indices: vec![] })
        .expect("decline");

    assert_eq!(k_count(runner.state()), 0);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "no second modal after decline"
    );
    assert!(runner
        .state()
        .active_repeated_optional_payment_frame()
        .is_none());
}

#[test]
fn afford_one_caps_modal_at_one_not_three() {
    let runner = hawkeye_runtime(1, &[]);
    assert_eq!(modal_cap(runner.state()), Some((0, 1)));
}

// ── Mode resolution through apply ─────────────────────────────────────────

#[test]
fn boomerang_mode_resolves_once_through_apply() {
    let mut runner = hawkeye_runtime(3, &["Mountain", "Forest"]);
    let hand_before = p0_hand_size(runner.state());
    assert_eq!(p0_pool_total(runner.state()), 3);

    runner
        .act(GameAction::SelectModes { indices: vec![2] })
        .expect("SelectModes accepted");

    assert_eq!(
        p0_pool_total(runner.state()),
        2,
        "Boomerang alone costs one {{1}}"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "no second modal after the single batched resolve"
    );
    assert_eq!(
        p0_hand_size(runner.state()),
        hand_before,
        "Boomerang discards one then draws one (net 0)"
    );
    assert!(runner
        .state()
        .active_repeated_optional_payment_frame()
        .is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn explosive_targeted_mode_resolves_once_through_apply() {
    let mut runner = hawkeye_runtime(3, &[]);
    assert_eq!(p1_life(runner.state()), 20);
    assert_eq!(p0_pool_total(runner.state()), 3);

    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("SelectModes accepted");

    assert_eq!(
        p0_pool_total(runner.state()),
        2,
        "Explosive alone costs one {{1}}"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Explosive needs a target player, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("ChooseTarget accepted");

    runner.advance_until_stack_empty();

    assert_eq!(
        p1_life(runner.state()),
        18,
        "Explosive deals exactly 2 damage once"
    );
}

#[test]
fn ability_mode_choice_with_mode_costs_survives_serde_roundtrip() {
    let mut runner = hawkeye_runtime(3, &[]);
    assert_eq!(modal_cap(runner.state()), Some((0, 3)));
    assert_eq!(mode_cost_count(runner.state()), Some(3));

    let v2 = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("batched AbilityModeChoice serializes as v2");
    assert_eq!(v2["resolution_state_version"], 2);
    let restored: GameState = serde_json::from_value::<ResolutionStateWire>(v2)
        .expect("batched AbilityModeChoice round-trips through the v2 wire")
        .into_game_state();

    assert_eq!(modal_cap(&restored), Some((0, 3)));
    assert_eq!(mode_cost_count(&restored), Some(3));
    assert!(
        repeated_payment_driver_is_pending(&restored),
        "batched pending payment survives serde"
    );

    *runner.state_mut() = restored;
    runner
        .act(GameAction::SelectModes {
            indices: vec![0, 1],
        })
        .expect("resume after roundtrip");
    assert_eq!(
        p0_pool_total(runner.state()),
        1,
        "two {{1}} payments survived serde resume"
    );
}

fn p0_hand_size(state: &GameState) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

fn p1_life(state: &GameState) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == P1)
        .map(|p| p.life)
        .unwrap_or(0)
}
