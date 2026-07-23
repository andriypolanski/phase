//! Issue #6498 — Portent of Calamity: revealed cards cannot be selected;
//! they just go to the graveyard.
//!
//! Oracle: `Reveal the top X cards of your library. For each card type, you
//! may exile a card of that type from among them. Put the rest into your
//! graveyard. You may cast a spell from among the exiled cards without paying
//! its mana cost if you exiled four or more cards this way. Then put the rest
//! of the exiled cards into your hand.`
//!
//! Discord report: after revealing, the player could not keep selected cards —
//! picks dumped to the graveyard. Root cause: "Put the rest into your
//! graveyard" was modeled as `ChangeZoneAll { Exile → Graveyard, TrackedSet }`,
//! so the per-type exile picks (the tracked set) were immediately moved to the
//! graveyard. Also, Dig→RevealTop demotion collapsed X to 1.
//!
//! DISCRIMINATING: with X=4 and four distinct types, exile one of each type;
//! those four stay in Exile (and later may go to hand); the unrevealed library
//! cards are untouched; non-exiled revealed cards go to the graveyard.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, Effect, ForEachCategoryAction, QuantityExpr, QuantityRef, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

const PORTENT: &str = "Reveal the top X cards of your library. For each card type, you may exile a card of that type from among them. Put the rest into your graveyard. You may cast a spell from among the exiled cards without paying its mana cost if you exiled four or more cards this way. Then put the rest of the exiled cards into your hand.";

fn add_mana(runner: &mut GameRunner, amount_blue: u32, amount_colorless: u32) {
    let dummy = ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..amount_blue {
        pool.add(ManaUnit::new(ManaType::Blue, dummy, false, vec![]));
    }
    for _ in 0..amount_colorless {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
}

#[test]
fn portent_parses_dynamic_reveal_and_last_revealed_rest_to_graveyard() {
    let def = parse_effect_chain(PORTENT, AbilityKind::Spell);
    // Head: Dig reveal with Variable X (not RevealTop count 1).
    match &*def.effect {
        Effect::Dig {
            reveal: true,
            keep_count: Some(0),
            count:
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable { name },
                },
            ..
        } => assert_eq!(name, "X"),
        Effect::RevealTop { count: 1, .. } => {
            panic!("X must not collapse to RevealTop {{ count: 1 }}")
        }
        other => panic!("expected reveal Dig with X, got {other:?}"),
    }

    let mut node = &def;
    let mut saw_for_each = false;
    let mut saw_rest_to_gy = false;
    loop {
        match &*node.effect {
            Effect::ForEachCategory {
                action: ForEachCategoryAction::ExileFromPool { .. },
                ..
            } => saw_for_each = true,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Library),
                destination: Zone::Graveyard,
                target: TargetFilter::LastRevealed,
                ..
            } => saw_rest_to_gy = true,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Graveyard,
                target: TargetFilter::TrackedSet { .. },
                ..
            } => panic!(
                "put-the-rest must NOT dump the exile tracked set into the graveyard (Discord #6498)"
            ),
            _ => {}
        }
        match node.sub_ability.as_deref() {
            Some(next) => node = next,
            None => break,
        }
    }
    assert!(saw_for_each, "must parse ForEachCategory exile");
    assert!(
        saw_rest_to_gy,
        "must emit ChangeZoneAll Library+LastRevealed→Graveyard for put-the-rest"
    );
}

#[test]
fn portent_exiled_picks_stay_exiled_not_dumped_to_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Portent of Calamity", false, PORTENT);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        });
        b.id()
    };
    let types = [
        CoreType::Creature,
        CoreType::Artifact,
        CoreType::Enchantment,
        CoreType::Sorcery,
    ];
    let mut lib_ids = Vec::new();
    for (i, ty) in types.iter().enumerate() {
        let id = scenario
            .add_spell_to_library_top(P0, &format!("Lib {i}"), true)
            .id();
        lib_ids.push((*ty, id));
    }
    let mut runner = scenario.build();
    for (ty, id) in &lib_ids {
        runner
            .state_mut()
            .objects
            .get_mut(id)
            .unwrap()
            .card_types
            .core_types = vec![*ty];
    }
    // X=4 + {U}{U}
    add_mana(&mut runner, 2, 4);

    let _outcome = runner.cast(spell).x(4).resolve();

    // Exile one card at each per-type prompt (four distinct types).
    let mut exiled = Vec::new();
    while let WaitingFor::ChooseFromZoneChoice { cards, .. } = &runner.state().waiting_for {
        let pick = cards[0];
        exiled.push(pick);
        runner
            .act(GameAction::SelectCards { cards: vec![pick] })
            .expect("per-type exile selection");
    }

    assert_eq!(
        exiled.len(),
        4,
        "must prompt once per distinct revealed type"
    );
    for id in &exiled {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Exile,
            "exiled picks must remain in Exile — not dumped to GY by put-the-rest"
        );
    }
    // Free-cast gate opens (4 exiled); decline or let the runner stop at the cast prompt.
    // Either way, the four must not be in the graveyard.
    for id in &exiled {
        assert!(
            !runner.state().players[0].graveyard.contains(id),
            "exiled pick must not be in the graveyard"
        );
    }
}
