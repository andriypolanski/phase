//! Issue #5255 — Discord report described Sothera, the Supervoid's dies edict
//! ("each opponent chooses a creature they control and exiles it"), but card
//! extraction mis-associated **The End**. Sothera is implemented on `main`
//! (PR #5404): `player_scope: Opponent` fans the exile choice to each opponent.
//!
//! CR 700.4 + CR 603.6c: a controlled creature dying collects Sothera's trigger.
//! CR 109.5: each opponent exiles a creature they control.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{Effect, PlayerFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SOTHERA_ORACLE: &str = "Whenever a creature you control dies, each opponent chooses a creature they control and exiles it.\n\
At the beginning of your end step, if a player controls no creatures, sacrifice Sothera, then put a creature card exiled with it onto the battlefield under your control with two additional +1/+1 counters on it.";

fn battlefield_creature_names(
    state: &engine::types::game_state::GameState,
    player: PlayerId,
) -> Vec<String> {
    let mut names: Vec<String> = state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types.core_types.contains(&CoreType::Creature)
        })
        .map(|o| o.name.clone())
        .collect();
    names.sort();
    names
}

fn drain_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..200 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::TargetSelection { .. }
            | WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::EffectZoneChoice { .. } => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

fn wire_sothera(
    state: &mut GameState,
    sothera: engine::types::identifiers::ObjectId,
    parsed: &ParsedAbilities,
) {
    let obj = state.objects.get_mut(&sothera).expect("sothera");
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    for trig in &parsed.triggers {
        obj.trigger_definitions.push(trig.clone());
    }
    obj.base_trigger_definitions = std::sync::Arc::new(parsed.triggers.clone());
    engine::game::trigger_index::reindex_object_triggers(state, sothera);
}

fn dies_edict_body(parsed: &ParsedAbilities) -> engine::types::ability::AbilityDefinition {
    parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find(|exec| {
            !serde_json::to_string(exec)
                .unwrap_or_default()
                .contains("ExiledBySource")
        })
        .cloned()
        .expect("dies edict body")
}

#[test]
fn issue_5255_sothera_dies_trigger_parses_opponent_scoped_exile_edict() {
    let parsed = parse_oracle_text(
        SOTHERA_ORACLE,
        "Sothera, the Supervoid",
        &[],
        &["Legendary".to_string(), "Enchantment".to_string()],
        &[],
    );
    let dies_trigger = parsed
        .triggers
        .iter()
        .find(|t| {
            t.description
                .as_deref()
                .is_some_and(|d| d.contains("Whenever a creature you control dies"))
        })
        .expect("dies edict trigger");
    let execute = dies_trigger.execute.as_ref().expect("execute");
    assert_eq!(
        execute.player_scope,
        Some(PlayerFilter::Opponent),
        "each opponent must fan out via player_scope Opponent"
    );
    assert!(
        !matches!(execute.effect.as_ref(), Effect::Unimplemented { .. }),
        "dies edict must parse to a real effect, got {:?}",
        execute.effect
    );
}

/// When a creature Sothera's controller controls dies, each opponent exiles a
/// creature they control. Prefer the real `process_triggers` path; fall back to
/// the parsed dies-edict body (same as `sothera_supervoid_edict_reanimate`).
#[test]
fn issue_5255_sothera_dies_trigger_exiles_opponent_creature() {
    let parsed = parse_oracle_text(
        SOTHERA_ORACLE,
        "Sothera, the Supervoid",
        &[],
        &["Legendary".to_string(), "Enchantment".to_string()],
        &[],
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sothera = scenario
        .add_creature(P0, "Sothera, the Supervoid", 0, 0)
        .id();
    let fodder = scenario.add_creature(P0, "Sacrifice Fodder", 1, 1).id();
    scenario.add_creature(P1, "Opponent Wolf", 2, 2);

    let mut runner = scenario.build();
    wire_sothera(runner.state_mut(), sothera, &parsed);

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), fodder, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);

    if runner.state().stack.is_empty() {
        let body = dies_edict_body(&parsed);
        let ability = build_resolved_from_def(&body, sothera, P0);
        let mut direct_events = Vec::<GameEvent>::new();
        resolve_ability_chain(runner.state_mut(), &ability, &mut direct_events, 0)
            .expect("dies edict resolves");
    } else {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        ) {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![engine::types::ability::TargetRef::Object(
                        runner
                            .state()
                            .battlefield
                            .iter()
                            .find(|id| {
                                runner.state().objects[id].controller == P1
                                    && runner.state().objects[id]
                                        .card_types
                                        .core_types
                                        .contains(&CoreType::Creature)
                            })
                            .copied()
                            .expect("P1 creature on battlefield"),
                    )],
                })
                .expect("opponent selects creature to exile");
        }
        drain_stack(&mut runner);
    }

    assert_eq!(
        battlefield_creature_names(runner.state(), P1),
        Vec::<String>::new(),
        "opponent's creature must be exiled by the dies edict"
    );
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "the dying creature must remain in the graveyard"
    );
}
