//! Regression for issue #5949: Vrestin, Menoptra Leader's attack trigger must
//! put a +1/+1 counter on EACH attacking Insect, not do nothing.
//!
//! "Whenever you attack with one or more Insects, put a +1/+1 counter on each
//! of them." — the "each of them" distributive refers to the declared-attackers
//! batch (CR 508.1 `AttackersDeclared`). The generic counter parser wrongly
//! promoted "on each ..." to the mass `PutCounterAll` form bound to the
//! single-object `TriggeringSource` anaphor, which matches nothing at
//! resolution, so the trigger "fired but did nothing." The fix excludes
//! `TargetFilter::ParentTarget` from the shared mass-classification branch in
//! `oracle_effect/mod.rs` (matching the imperative authority), keeping
//! `PutCounter { target: ParentTarget }` so resolution binds the bounded batch
//! via `parent_target_refs_from_attack_trigger_context`.
//!
//! https://github.com/phase-rs/phase/issues/5949

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::AttackTarget;

const ATTACK_TRIGGER: &str =
    "Whenever you attack with one or more Insects, put a +1/+1 counter on each of them.";

const VRESTIN_ORACLE: &str = "Flying\n\
Vrestin enters with X +1/+1 counters on it.\n\
When Vrestin enters, create X 1/1 green and white Alien Insect creature tokens with flying.\n\
Whenever you attack with one or more Insects, put a +1/+1 counter on each of them.";

#[test]
fn vrestin_attack_trigger_parses_as_parent_target_put_counter() {
    let parsed = parse_oracle_text(VRESTIN_ORACLE, "Vrestin, Menoptra Leader", &[], &[], &[]);
    let attack_trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::YouAttack)
        .expect("Vrestin should have a YouAttack trigger");
    assert!(
        attack_trigger.batched,
        "'one or more Insects' is a batched attack trigger"
    );
    let execute = attack_trigger.execute.as_ref().expect("execute");
    match execute.effect.as_ref() {
        // CR 508.1 + CR 608.2c: the batch anaphor "each of them" must lower to
        // the singular PutCounter over the ParentTarget batch — NOT a mass
        // PutCounterAll bound to the single-object TriggeringSource anaphor.
        Effect::PutCounter { target, .. } => {
            assert_eq!(*target, TargetFilter::ParentTarget);
        }
        other => panic!("expected PutCounter {{ target: ParentTarget }}, got {other:?}"),
    }
}

fn resolve_attack_triggers(runner: &mut GameRunner) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..count).collect(),
                    })
                    .expect("order triggers");
            }
            other => panic!("unexpected waiting state during attack triggers: {other:?}"),
        }
    }
    panic!("attack triggers did not resolve");
}

fn plus1_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object exists")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn vrestin_puts_counter_on_each_attacking_insect_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Vrestin (the ability source) — an Insect that carries the attack trigger.
    let vrestin = scenario
        .add_creature(P0, "Vrestin, Menoptra Leader", 2, 2)
        .with_subtypes(vec!["Insect"])
        .from_oracle_text(ATTACK_TRIGGER)
        .id();

    // Two more Insects controlled by the attacking player.
    let insect_a = scenario
        .add_creature(P0, "Alien Insect A", 1, 1)
        .with_subtypes(vec!["Insect"])
        .id();
    let insect_b = scenario
        .add_creature(P0, "Alien Insect B", 1, 1)
        .with_subtypes(vec!["Insect"])
        .id();

    // A non-Insect attacker — must NOT receive a counter (CR 508.1 batch is
    // gated by the trigger's Insect subject filter).
    let bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();

    let attack_pairs: Vec<_> = [vrestin, insect_a, insect_b, bear]
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attack_pairs)
        .expect("declare attackers");

    resolve_attack_triggers(&mut runner);

    assert_eq!(
        plus1_counters(&runner, vrestin),
        1,
        "Vrestin (attacking Insect) must get a +1/+1 counter"
    );
    assert_eq!(
        plus1_counters(&runner, insect_a),
        1,
        "attacking Insect A must get a +1/+1 counter"
    );
    assert_eq!(
        plus1_counters(&runner, insect_b),
        1,
        "attacking Insect B must get a +1/+1 counter"
    );
    assert_eq!(
        plus1_counters(&runner, bear),
        0,
        "the non-Insect attacker must NOT get a +1/+1 counter"
    );
}
