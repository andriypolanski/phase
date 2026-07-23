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
//! DISCRIMINATING: with X=5, exile one of each of four types plus a duplicate
//! creature; the unselected revealed creature goes to the graveyard; after
//! declining the free cast, the four exiled picks reach hand via
//! `TrackedSetFiltered { caused_by: Exiled }`, not chain `TrackedSet`.

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
    let mut saw_final_hand_cleanup = false;
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
                destination: Zone::Hand,
                target: TargetFilter::TrackedSetFiltered {
                    caused_by: Some(engine::types::ability::ThisWayCause::Exiled),
                    ..
                },
                ..
            } => saw_final_hand_cleanup = true,
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
    assert!(
        saw_final_hand_cleanup,
        "final tail must bind to action-stamped TrackedSetFiltered(Exiled), not chain TrackedSet"
    );

    let mut node = &def;
    let cast = loop {
        if matches!(&*node.effect, Effect::CastFromZone { .. }) {
            break node;
        }
        node = node
            .sub_ability
            .as_ref()
            .expect("Portent chain must reach CastFromZone");
    };
    let hand_cleanup = |node: &engine::types::ability::AbilityDefinition| {
        matches!(
            &*node.effect,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Hand,
                target: TargetFilter::TrackedSetFiltered {
                    caused_by: Some(engine::types::ability::ThisWayCause::Exiled),
                    ..
                },
                ..
            }
        )
    };
    assert!(
        cast.sub_ability.as_deref().is_some_and(hand_cleanup)
            || cast.else_ability.as_deref().is_some_and(hand_cleanup),
        "CastFromZone must chain into the exiled-card hand cleanup; sub={:?}, else={:?}",
        cast.sub_ability.as_ref().map(|s| &*s.effect),
        cast.else_ability.as_ref().map(|s| &*s.effect),
    );
    assert!(
        cast.sub_ability
            .as_deref()
            .is_some_and(hand_cleanup),
        "hand cleanup must be the accept/decline sub_ability (SequentialSibling), not only else_ability"
    );
    assert_eq!(
        cast.sub_ability.as_ref().unwrap().sub_link,
        engine::types::ability::SubAbilityLink::SequentialSibling,
        "Portent hand cleanup must resolve on optional cast decline"
    );
}

#[test]
fn dynamic_reveal_put_rest_emits_last_revealed_sibling() {
    // Shared grammar class from the parse-diff (Sunbird's Invocation tail,
    // Enshrined Memories-style separate rest clause, etc.): dynamic-count
    // reveal-only Dig + trailing "put the rest …".
    const DYNAMIC_REVEAL_REST: &str = "Reveal the top X cards of your library. Put the rest on the bottom of your library in any order.";
    let def = parse_effect_chain(DYNAMIC_REVEAL_REST, AbilityKind::Spell);
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
        other => panic!("expected dynamic reveal Dig with X, got {other:?}"),
    }

    let mut node = &def;
    let mut saw_last_revealed_rest = false;
    loop {
        if matches!(
            &*node.effect,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Library),
                destination: Zone::Library,
                target: TargetFilter::LastRevealed,
                ..
            }
        ) {
            saw_last_revealed_rest = true;
        }
        match node.sub_ability.as_deref() {
            Some(next) => node = next,
            None => break,
        }
    }
    assert!(
        saw_last_revealed_rest,
        "dynamic reveal Dig with put-the-rest must emit explicit LastRevealed sibling \
         instead of relying on unused Dig.rest_destination"
    );
}

#[test]
fn dynamic_reveal_put_rest_moves_revealed_cards_to_library_bottom() {
    // CR 701.20a + CR 608.2c + CR 401.4: dynamic reveal-only Dig with a trailing
    // put-the-rest clause must move the revealed library remainder to the bottom,
    // leaving cards below the reveal window untouched (Enshrined Memories class).
    const DYNAMIC_REVEAL_REST: &str = "Reveal the top X cards of your library. Put the rest on the bottom of your library in any order.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Library top-to-bottom: [rev3, rev2, rev1, deep]. X=3 reveals the top three.
    let deep = scenario
        .add_spell_to_library_top(P0, "Deep Card", true)
        .id();
    let rev1 = scenario
        .add_spell_to_library_top(P0, "Revealed 1", true)
        .id();
    let rev2 = scenario
        .add_spell_to_library_top(P0, "Revealed 2", true)
        .id();
    let rev3 = scenario
        .add_spell_to_library_top(P0, "Revealed 3", true)
        .id();

    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Dynamic Reveal Probe",
            false,
            DYNAMIC_REVEAL_REST,
        );
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        });
        b.id()
    };

    let mut runner = scenario.build();
    add_mana(&mut runner, 0, 3);

    // CR 401.4: submit a non-default bottom order through the production
    // `EffectZoneChoice` path — not an engine-default batch order.
    let outcome = runner
        .cast(spell)
        .x(3)
        .effect_zone(&[rev3, rev1, rev2])
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "dynamic reveal rest cleanup must finish after the library-order choice"
    );

    let library = &runner.state().players[0].library;
    assert_eq!(
        library.len(),
        4,
        "spell must not remove the unrevealed fourth card from the library"
    );
    assert_eq!(
        library[0], deep,
        "the card below the X-card reveal window must remain on top"
    );
    let bottom_tail: Vec<ObjectId> = library.iter().skip(1).copied().collect();
    assert_eq!(
        bottom_tail,
        vec![rev3, rev1, rev2],
        "revealed remainder must land on the bottom in the player's submitted order"
    );
    for id in [rev1, rev2, rev3] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Library,
            "revealed cards must stay in the library, not be stranded elsewhere"
        );
    }
}

#[test]
fn portent_full_resolution_exiles_picks_to_hand_and_unselected_reveal_to_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Portent of Calamity", false, PORTENT);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
            generic: 0,
        });
        b.id()
    };

    // Five revealed cards: four distinct types plus a duplicate creature.
    let creature_a = scenario
        .add_spell_to_library_top(P0, "Creature A", true)
        .id();
    let creature_b = scenario
        .add_spell_to_library_top(P0, "Creature B", true)
        .id();
    let artifact = scenario.add_spell_to_library_top(P0, "Artifact", true).id();
    let enchantment = scenario
        .add_spell_to_library_top(P0, "Enchantment", true)
        .id();
    let sorcery = scenario.add_spell_to_library_top(P0, "Sorcery", true).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&creature_a)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];
    runner
        .state_mut()
        .objects
        .get_mut(&creature_b)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];
    runner
        .state_mut()
        .objects
        .get_mut(&artifact)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Artifact];
    runner
        .state_mut()
        .objects
        .get_mut(&enchantment)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Enchantment];
    runner
        .state_mut()
        .objects
        .get_mut(&sorcery)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Sorcery];

    // X=5 + {U}
    add_mana(&mut runner, 1, 5);

    let _outcome = runner.cast(spell).x(5).resolve();

    let mut exiled = Vec::new();
    while let WaitingFor::ChooseFromZoneChoice { cards, .. } = &runner.state().waiting_for {
        let pick = if cards.contains(&creature_a) && cards.contains(&creature_b) {
            creature_a
        } else {
            cards[0]
        };
        exiled.push(pick);
        runner
            .act(GameAction::SelectCards { cards: vec![pick] })
            .expect("per-type exile selection");
    }

    assert_eq!(
        exiled.len(),
        4,
        "must exile exactly one card per distinct revealed type"
    );
    for id in &exiled {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Exile,
            "exiled picks must remain in Exile through the revealed rest cleanup"
        );
    }

    let unselected_creature = if exiled.contains(&creature_a) {
        creature_b
    } else {
        creature_a
    };
    assert_eq!(
        runner.state().objects[&unselected_creature].zone,
        Zone::Graveyard,
        "the revealed-but-unselected duplicate creature must go to the graveyard"
    );

    // Decline the optional free cast, then drive the final hand cleanup.
    while !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline optional free cast");
            }
            WaitingFor::CastOffer { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("decline cast offer");
            }
            other => panic!("unexpected prompt before final cleanup: {other:?}"),
        }
    }

    for id in &exiled {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Hand,
            "remaining exiled cards must reach hand via TrackedSetFiltered(Exiled) tail"
        );
        assert!(
            !runner.state().players[0].graveyard.contains(id),
            "exiled pick must not be stranded in the graveyard"
        );
    }
}
