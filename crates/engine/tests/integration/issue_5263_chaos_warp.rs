//! Issue #5263 — Chaos Warp must shuffle the targeted permanent into its
//! owner's library, then reveal the top of that owner's library (not the
//! caster's). Fixed on `main` in PR #5234 (#2406 owner-library routing).
//!
//! CR 400.3 + CR 400.7j: zone changes and reveals follow the targeted
//! permanent's owner, not the spell's controller.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CHAOS_WARP_ORACLE: &str = "The owner of target permanent shuffles it into their library, then reveals the top card of their library. If it's a permanent card, they put it onto the battlefield.";

fn put_library_top(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let owner = runner.state().objects.get(&id).expect("object").owner;
    let zone = runner.state().objects.get(&id).expect("object").zone;
    let mut events = Vec::new();
    if zone != Zone::Library {
        engine::game::zones::remove_from_zone(runner.state_mut(), id, zone, owner);
        runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Library;
        runner
            .state_mut()
            .players
            .get_mut(owner.0 as usize)
            .unwrap()
            .library
            .push_back(id);
    }
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

#[test]
fn chaos_warp_parse_oracle_text_owner_library_shuffle_reveal_chain() {
    let parsed = parse_oracle_text(
        CHAOS_WARP_ORACLE,
        "Chaos Warp",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = parsed.abilities.first().expect("spell ability");
    let Effect::ChangeZone {
        owner_library: true,
        destination,
        ..
    } = ability.effect.as_ref()
    else {
        panic!("expected owner-library ChangeZone, got {:?}", ability.effect);
    };
    assert_eq!(*destination, Zone::Library);

    let shuffle = ability.sub_ability.as_ref().expect("shuffle sub");
    assert_eq!(
        shuffle.effect.target_filter(),
        Some(&TargetFilter::ParentTargetOwner)
    );

    let reveal = shuffle
        .sub_ability
        .as_ref()
        .expect("reveal sub")
        .effect
        .as_ref();
    let Effect::RevealTop {
        player,
        count: 1,
    } = reveal
    else {
        panic!("expected RevealTop, got {reveal:?}");
    };
    assert_eq!(*player, TargetFilter::ParentTargetOwner);
}

#[test]
fn chaos_warp_cast_reveals_target_owner_library_not_caster() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let owner_library_creature = scenario.add_creature(P1, "Owner Library Bear", 3, 3).id();
    let caster_library_instant = scenario
        .add_spell_to_library_top(P0, "Caster Top Bolt", true)
        .id();

    let warp = scenario
        .add_spell_to_hand_from_oracle(P0, "Chaos Warp", true, CHAOS_WARP_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, warp, false, vec![]),
            ManaUnit::new(ManaType::Colorless, warp, false, vec![]),
            ManaUnit::new(ManaType::Red, warp, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    put_library_top(&mut runner, owner_library_creature);

    runner
        .cast(warp)
        .target_object(target)
        .resolve();

    assert_eq!(
        runner.state().objects[&caster_library_instant].zone,
        Zone::Library,
        "caster's library top must not be consumed by Chaos Warp reveal"
    );
    assert!(
        !runner.state().last_revealed_ids.contains(&caster_library_instant),
        "Chaos Warp must reveal the target owner's library, not the caster's"
    );
    assert!(
        runner
            .state()
            .last_revealed_ids
            .iter()
            .all(|id| runner.state().objects[id].owner == P1),
        "revealed cards must belong to the targeted permanent's owner (P1)"
    );
}
