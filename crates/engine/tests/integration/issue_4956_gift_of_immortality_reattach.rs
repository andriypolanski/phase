//! Issue #4956: Gift of Immortality delayed Aura reattach must specify the
//! host ("that creature" / "you") instead of opening CR 303.4f Aura choice.
//!
//! Peers: Next of Kin (attach to the put creature), Lynde (attach to you).

use engine::game::effects::attach::{attach_to, attach_to_player};
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, DelayedTriggerCondition, Effect, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GIFT_ORACLE: &str = "Enchant creature\n\
When enchanted creature dies, return that card to the battlefield under its \
owner's control. Return this card to the battlefield attached to that creature \
at the beginning of the next end step.";

const NEXT_OF_KIN_ORACLE: &str = "Enchant creature\n\
When enchanted creature dies, you may put a creature card you own with lesser \
mana value from your hand or from the command zone onto the battlefield. If \
you do, return this card to the battlefield attached to that creature at the \
beginning of the next end step.";

const LYNDE_ORACLE: &str = "Deathtouch\n\
Whenever a Curse is put into your graveyard from the battlefield, return it to \
the battlefield attached to you at the beginning of the next end step.\n\
At the beginning of your upkeep, you may attach a Curse attached to you to one \
of your opponents. If you do, draw two cards.";

const SMOKE_SHROUD_ORACLE: &str = "Enchant creature\n\
Enchanted creature gets +1/+1 and has flying.\n\
When a Ninja you control enters, you may return this card from your graveyard \
to the battlefield attached to that creature.";

const DRAGON_BREATH_ORACLE: &str = "Enchant creature\n\
Enchanted creature has haste.\n\
{R}: Enchanted creature gets +1/+0 until end of turn.\n\
When a creature with mana value 6 or greater enters, you may return this card \
from your graveyard to the battlefield attached to that creature.";

const CASS_ORACLE: &str = "Vigilance\n\
Whenever Cass or another creature you control dies, if it was enchanted or \
equipped, return any number of Aura cards that were attached to it from your \
graveyard to the battlefield attached to target creature, then attach any \
number of Equipment that were attached to it to that creature.";

const STORM_HERALD_ORACLE: &str = "Haste\n\
When this creature enters, return any number of Aura cards from your graveyard \
to the battlefield attached to creatures you control. Exile those Auras at the \
beginning of your next end step. If those Auras would leave the battlefield, \
exile them instead of putting them anywhere else.";

/// Event-subject GY return with nested Attach→ParentTarget (Smoke Shroud / Dragon Breath).
fn event_subject_return_attach_host(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> &TargetFilter {
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| {
            matches!(
                t.execute.as_ref().map(|e| e.effect.as_ref()),
                Some(Effect::ChangeZone {
                    destination: Zone::Battlefield,
                    ..
                })
            )
        })
        .expect("GY return trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(
        execute.forward_result,
        "event-subject return must stamp forward_result for Attach nest"
    );
    let attach = execute.sub_ability.as_ref().expect("Attach nest");
    match attach.effect.as_ref() {
        Effect::Attach {
            attachment: TargetFilter::SelfRef,
            target,
        } => target,
        other => panic!("expected Attach SelfRef→host, got {other:?}"),
    }
}

fn ability_chain_contains_equipment_attach(
    def: &engine::types::ability::AbilityDefinition,
) -> bool {
    let is_equipment_attach = matches!(
        def.effect.as_ref(),
        Effect::Attach {
            attachment: TargetFilter::Typed(tf),
            ..
        } if tf.type_filters.iter().any(|f| {
            matches!(f, engine::types::ability::TypeFilter::Subtype(s) if s == "Equipment")
        })
    );
    is_equipment_attach
        || def
            .sub_ability
            .as_deref()
            .is_some_and(ability_chain_contains_equipment_attach)
        || def
            .else_ability
            .as_deref()
            .is_some_and(ability_chain_contains_equipment_attach)
}

fn ability_chain_contains_delayed_exile(def: &engine::types::ability::AbilityDefinition) -> bool {
    let is_exile = match def.effect.as_ref() {
        Effect::CreateDelayedTrigger { effect, .. } => {
            matches!(
                effect.effect.as_ref(),
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            ) || ability_chain_contains_delayed_exile(effect)
        }
        Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        } => true,
        _ => false,
    };
    is_exile
        || def
            .sub_ability
            .as_deref()
            .is_some_and(ability_chain_contains_delayed_exile)
        || def
            .else_ability
            .as_deref()
            .is_some_and(ability_chain_contains_delayed_exile)
}

fn effect_is_unimplemented(effect: &Effect) -> bool {
    matches!(effect, Effect::Unimplemented { .. })
}

fn count_cdts(def: &engine::types::ability::AbilityDefinition) -> usize {
    let head = matches!(def.effect.as_ref(), Effect::CreateDelayedTrigger { .. }) as usize;
    head + def.sub_ability.as_deref().map(count_cdts).unwrap_or(0)
        + def.else_ability.as_deref().map(count_cdts).unwrap_or(0)
}

fn gift_delayed_attach_host(parsed: &engine::parser::oracle::ParsedAbilities) -> &TargetFilter {
    let trigger = parsed.triggers.first().expect("dies trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    let cdt = execute
        .sub_ability
        .as_ref()
        .expect("CreateDelayedTrigger sibling");
    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = cdt.effect.as_ref()
    else {
        panic!("expected CreateDelayedTrigger, got {:?}", cdt.effect);
    };
    assert_eq!(
        condition,
        &DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    let inner = effect.as_ref();
    assert!(
        inner.forward_result,
        "delayed ChangeZone must forward_result into Attach"
    );
    let attach = inner.sub_ability.as_ref().expect("Attach nest");
    match (inner.effect.as_ref(), attach.effect.as_ref()) {
        (
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                ..
            },
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target,
            },
        ) => target,
        other => panic!("unexpected Gift delayed body shape: {other:?}"),
    }
}

fn drain_priority(runner: &mut GameRunner) {
    for _ in 0..256 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::ReturnAsAuraTarget { .. } => {
                panic!(
                    "CR 303.4f Aura host choice must not open when attach_to is specified; \
                     waiting_for = {:?}",
                    runner.state().waiting_for
                );
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional");
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                let pick = cards.first().copied().expect("zone choice candidate");
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("choose put creature");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
    panic!(
        "drain_priority exceeded bound; waiting_for = {:?}",
        runner.state().waiting_for
    );
}

fn advance_through_delayed_end(runner: &mut GameRunner) {
    for _ in 0..256 {
        // Stop once the delayed trigger has fired and End/Cleanup priority is
        // idle — do not keep walking into later turns.
        if runner.state().delayed_triggers.is_empty()
            && runner.state().stack.is_empty()
            && matches!(runner.state().phase, Phase::End | Phase::Cleanup)
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::ReturnAsAuraTarget { .. } => {
                panic!(
                    "delayed reattach must not prompt for Aura host; waiting_for = {:?}",
                    runner.state().waiting_for
                );
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareAttackers { .. } => {
                // Lynde (and similar) can be a legal attacker; empty declaration
                // lets auto-advance reach the End step where the delayed fires.
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    panic!(
                        "priority pass stalled; phase={:?} dt={} stack={} wf={:?}",
                        runner.state().phase,
                        runner.state().delayed_triggers.len(),
                        runner.state().stack.len(),
                        runner.state().waiting_for,
                    );
                }
            }
        }
    }
    panic!(
        "advance_through_delayed_end exceeded bound; phase = {:?}, dt = {}, stack = {}, wf = {:?}",
        runner.state().phase,
        runner.state().delayed_triggers.len(),
        runner.state().stack.len(),
        runner.state().waiting_for
    );
}

#[test]
fn gift_of_immortality_delayed_reattach_shape() {
    let parsed = parse_oracle_text(
        GIFT_ORACLE,
        "Gift of Immortality",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1);
    assert!(
        !effect_is_unimplemented(&parsed.triggers[0].execute.as_ref().unwrap().effect),
        "Gift dies trigger must be supported"
    );
    assert_eq!(
        gift_delayed_attach_host(&parsed),
        &TargetFilter::ParentTarget,
        "Gift delayed Attach host must be ParentTarget (that creature)"
    );
}

#[test]
fn next_of_kin_delayed_reattach_shape() {
    let parsed = parse_oracle_text(
        NEXT_OF_KIN_ORACLE,
        "Next of Kin",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let trigger = parsed.triggers.first().expect("dies trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(
        !effect_is_unimplemented(&execute.effect),
        "Next of Kin must parse supported: {:?}",
        execute.effect
    );
    let delayed_link = execute
        .sub_ability
        .as_ref()
        .expect("delayed / if-you-do sibling");
    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = delayed_link.effect.as_ref()
    else {
        panic!(
            "Next of Kin 'next end step' must wrap CreateDelayedTrigger (short-name \
             'next' must not rewrite temporal text); got {:?}",
            delayed_link.effect
        );
    };
    assert_eq!(
        condition,
        &DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    let inner = effect.as_ref();
    assert!(inner.forward_result);
    // "If you do" gates CreateDelayedTrigger installation, not the delayed body
    // at end-step fire time (OptionalEffectPerformed is a creation-time signal).
    assert_eq!(
        delayed_link.condition.as_ref(),
        Some(&AbilityCondition::effect_performed()),
        "OptionalEffectPerformed must lift onto the CreateDelayedTrigger wrapper"
    );
    assert!(
        inner.condition.is_none(),
        "delayed payload must not retain OptionalEffectPerformed; got {:?}",
        inner.condition
    );
    let attach = inner.sub_ability.as_ref().expect("Attach nest");
    match attach.effect.as_ref() {
        Effect::Attach {
            attachment,
            target: TargetFilter::ParentTarget,
        } => {
            assert_eq!(
                attachment,
                &TargetFilter::SelfRef,
                "Next of Kin Attach attachment must be SelfRef, got {attachment:?}"
            );
        }
        other => panic!("Next of Kin Attach host must be ParentTarget, got {other:?}"),
    }
    assert_eq!(
        count_cdts(execute),
        1,
        "nest-before-wrap must yield a single CreateDelayedTrigger, not two"
    );
}

#[test]
fn lynde_delayed_reattach_shape() {
    let parsed = parse_oracle_text(
        LYNDE_ORACLE,
        "Lynde, Cheerful Tormentor",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Warlock".to_string()],
    );
    let curse_ltb = parsed
        .triggers
        .iter()
        .find(|t| {
            matches!(
                t.execute.as_ref().map(|e| e.effect.as_ref()),
                Some(Effect::CreateDelayedTrigger { .. })
            )
        })
        .expect("Curse LTB delayed trigger");
    let execute = curse_ltb.execute.as_ref().unwrap();
    let Effect::CreateDelayedTrigger {
        effect, condition, ..
    } = execute.effect.as_ref()
    else {
        unreachable!()
    };
    assert_eq!(
        condition,
        &DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    let inner = effect.as_ref();
    assert!(inner.forward_result);
    let attach = inner.sub_ability.as_ref().expect("Attach nest");
    match (inner.effect.as_ref(), attach.effect.as_ref()) {
        (
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                target: TargetFilter::TriggeringSource,
                ..
            },
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::Controller,
            },
        ) => {}
        other => panic!("Lynde delayed body shape wrong: {other:?}"),
    }
}

#[test]
fn gift_of_immortality_reattaches_without_aura_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let gift = scenario
        .add_creature(P0, "Gift of Immortality", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(GIFT_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();
    // `attach_to` returns the prior host (`None` on first attach), not success.
    attach_to(runner.state_mut(), gift, host);
    assert_eq!(
        runner.state().objects[&gift].attached_to,
        Some(AttachTarget::Object(host)),
        "Gift must start attached to the host"
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Battlefield,
        "dies trigger returns the enchanted creature"
    );
    assert_eq!(
        runner.state().objects[&gift].zone,
        Zone::Graveyard,
        "Gift is in the graveyard awaiting end-step return"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "exactly one AtNextEnd delayed reattach must be installed"
    );
    assert_eq!(
        runner.state().delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(host)],
        "delayed trigger must snapshot the returned creature for ParentTarget Attach; \
         targets={:?} source={:?} effect={:?}",
        runner.state().delayed_triggers[0].ability.targets,
        runner.state().delayed_triggers[0].ability.source_id,
        runner.state().delayed_triggers[0].ability.effect,
    );
    assert_eq!(
        runner.state().delayed_triggers[0].ability.source_id,
        gift,
        "delayed SelfRef source must remain Gift, not the returned creature"
    );
    assert!(
        matches!(
            &runner.state().delayed_triggers[0].ability.effect,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                origin: None,
                ..
            }
        ),
        "delayed ChangeZone must be SelfRef→BF with no origin guard; got {:?}",
        runner.state().delayed_triggers[0].ability.effect
    );
    assert!(
        matches!(
            runner.state().delayed_triggers[0]
                .ability
                .sub_ability
                .as_ref()
                .map(|s| &s.effect),
            Some(Effect::Attach {
                target: TargetFilter::ParentTarget,
                ..
            })
        ),
        "delayed body must nest Attach→ParentTarget; sub={:?}",
        runner.state().delayed_triggers[0].ability.sub_ability
    );
    runner.advance_to_end_step();
    advance_through_delayed_end(&mut runner);

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "delayed reattach must have fired; phase={:?} dt={:?} stack={} wf={:?}",
        runner.state().phase,
        runner.state().delayed_triggers.len(),
        runner.state().stack.len(),
        runner.state().waiting_for,
    );
    let gift_obj = &runner.state().objects[&gift];
    assert_eq!(
        gift_obj.zone,
        Zone::Battlefield,
        "Gift returns at the next end step; attached={:?} core={:?} subtypes={:?} kw={:?} host_zone={:?} wf={:?}",
        gift_obj.attached_to,
        gift_obj.card_types.core_types,
        gift_obj.card_types.subtypes,
        gift_obj.keywords,
        runner.state().objects[&host].zone,
        runner.state().waiting_for,
    );
    assert_eq!(
        runner.state().objects[&gift].attached_to,
        Some(AttachTarget::Object(host)),
        "Gift auto-attaches to that creature — no CR 303.4f prompt"
    );
}

#[test]
fn gift_of_immortality_stays_in_graveyard_when_host_gone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let gift = scenario
        .add_creature(P0, "Gift of Immortality", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(GIFT_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), gift, host);
    assert_eq!(
        runner.state().objects[&gift].attached_to,
        Some(AttachTarget::Object(host))
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "hostile path must install the delayed reattach before the host is exiled; \
         otherwise the negative assertion can pass without exercising CR 303.4i"
    );
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Battlefield,
        "dies trigger must return the host before the hostile exile"
    );

    // CR 303.4i + Gatherer: exile the returned host before end step → Gift remains in GY.
    let mut exile_events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Exile, &mut exile_events);

    runner.advance_to_end_step();
    advance_through_delayed_end(&mut runner);

    assert_eq!(
        runner.state().objects[&gift].zone,
        Zone::Graveyard,
        "Gift remains in the graveyard when the specified host is undefined/illegal"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "must not open Aura host choice when the specified host is gone"
    );
}

#[test]
fn next_of_kin_attaches_to_put_creature_not_dying_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Dying host A (MV 3); put creature B from hand (MV 2, lesser).
    let host_a = scenario
        .add_creature(P0, "Hill Giant", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 3,
        })
        .id();
    let put_b = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .id();
    let aura = scenario
        .add_creature(P0, "Next of Kin", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(NEXT_OF_KIN_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, host_a);
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host_a))
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host_a, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&put_b].zone,
        Zone::Battlefield,
        "Next of Kin puts the lesser-MV creature"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "if-you-do delayed reattach installed"
    );
    assert!(
        runner.state().delayed_triggers[0]
            .ability
            .condition
            .is_none(),
        "installed delayed body must not carry OptionalEffectPerformed; got {:?}",
        runner.state().delayed_triggers[0].ability.condition
    );

    runner.advance_to_end_step();
    advance_through_delayed_end(&mut runner);

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Battlefield,
        "Next of Kin returns at end step"
    );
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(put_b)),
        "hostile: attach to put creature B, not dying host A"
    );
}

#[test]
fn lynde_returns_curse_attached_to_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _lynde = scenario
        .add_creature(P0, "Lynde, Cheerful Tormentor", 2, 4)
        .from_oracle_text(LYNDE_ORACLE)
        .id();
    let curse = scenario
        .add_creature(P0, "Curse of Thirst", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura", "Curse"])
        .with_keyword(Keyword::Enchant(TargetFilter::Player))
        .id();

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P0);
    assert_eq!(
        runner.state().objects[&curse].attached_to,
        Some(AttachTarget::Player(P0))
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), curse, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(runner.state().delayed_triggers.len(), 1);

    runner.advance_to_end_step();
    advance_through_delayed_end(&mut runner);

    assert_eq!(runner.state().objects[&curse].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&curse].attached_to,
        Some(AttachTarget::Player(P0)),
        "Lynde returns the Curse attached to you"
    );
}

#[test]
fn smoke_shroud_and_dragon_breath_attach_host_is_parent_target() {
    for (name, oracle) in [
        ("Smoke Shroud", SMOKE_SHROUD_ORACLE),
        ("Dragon Breath", DRAGON_BREATH_ORACLE),
    ] {
        let parsed = parse_oracle_text(
            oracle,
            name,
            &[],
            &["Enchantment".to_string()],
            &["Aura".to_string()],
        );
        assert_eq!(
            event_subject_return_attach_host(&parsed),
            &TargetFilter::ParentTarget,
            "{name}: GY return Attach host must be ParentTarget (that creature)"
        );
    }
}

#[test]
fn cass_preserves_equipment_reattach_continuation() {
    let parsed = parse_oracle_text(
        CASS_ORACLE,
        "Cass, Hand of Vengeance",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Assassin".to_string()],
    );
    let execute = parsed
        .triggers
        .first()
        .and_then(|t| t.execute.as_ref())
        .expect("Cass dies trigger");
    assert!(
        ability_chain_contains_equipment_attach(execute),
        "Cass ', then attach any number of Equipment …' must survive attach-host parse; \
         execute={:?}",
        execute.effect
    );
}

#[test]
fn storm_herald_preserves_delayed_exile_continuation() {
    let parsed = parse_oracle_text(
        STORM_HERALD_ORACLE,
        "Storm Herald",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Shaman".to_string()],
    );
    let execute = parsed
        .triggers
        .first()
        .and_then(|t| t.execute.as_ref())
        .expect("Storm Herald ETB");
    assert!(
        ability_chain_contains_delayed_exile(execute),
        "Storm Herald '. Exile those Auras …' must survive attach-host parse; execute={:?}",
        execute.effect
    );
}

#[test]
fn smoke_shroud_attaches_to_entering_ninja_among_multiple_hosts() {
    // CR 303.4f + CR 608.2c: with another legal Aura host on the battlefield,
    // the event-subject return must bind ParentTarget to the entering Ninja —
    // no CR 303.4f prompt, and not the distractor creature.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let distractor = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let ninja = scenario
        .add_creature_to_hand(P0, "Ninja of the Deep Hours", 2, 2)
        .with_subtypes(vec!["Human", "Ninja"])
        .id();
    let aura = scenario
        .add_creature(P0, "Smoke Shroud", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(SMOKE_SHROUD_ORACLE)
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();

    let mut runner = scenario.build();
    // Prior host so AttachedTo fallback would prefer the distractor if the
    // event referent is not hydrated onto the nested Attach.
    attach_to(runner.state_mut(), aura, distractor);
    let mut gy_events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Graveyard, &mut gy_events);
    process_triggers(runner.state_mut(), &gy_events);
    drain_priority(&mut runner);
    assert_eq!(runner.state().objects[&aura].zone, Zone::Graveyard);

    let mut etb_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        ninja,
        Zone::Battlefield,
        &mut etb_events,
    );
    process_triggers(runner.state_mut(), &etb_events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Battlefield,
        "Smoke Shroud returns from GY on Ninja ETB"
    );
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(ninja)),
        "must attach to the entering Ninja, not the distractor ({distractor:?}); \
         attached_to={:?}",
        runner.state().objects[&aura].attached_to
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "must not open CR 303.4f Aura host choice among multiple legal hosts"
    );
}
