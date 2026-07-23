//! Issue #6499 — Flickering Ward cannot stay attached after choosing a color.
//!
//! Oracle: `Enchant creature` / `As this Aura enters, choose a color.` /
//! `Enchanted creature has protection from the chosen color. This effect
//! doesn't remove this Aura.` / `{W}: Return this Aura to its owner's hand.`
//!
//! Discord report: after picking a color, the Aura would not stay attached.
//! Choosing white (the Aura's color) grants protection from white; without
//! CR 702.16n's "doesn't remove this Aura" rider, SBA CR 704.5m / CR 702.16c
//! puts the white Aura into the graveyard.
//!
//! Root cause: the parser dropped the rider as "inert prose" so coverage
//! claimed the protection grant was supported while the exemption was never
//! modeled. Fix stamps `ProtectionDoesNotRemove::Source` on the continuous
//! static and honors it in `attachment_illegality`.
//!
//! DISCRIMINATING: with chosen color = white, Flickering Ward stays attached
//! and on the battlefield. A revert (no rider / no exemption check) sends it
//! to the graveyard.

use engine::game::layers::evaluate_layers;
use engine::game::sba::check_state_based_actions;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ChosenAttribute, ContinuousModification, ProtectionDoesNotRemove};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::ManaColor;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const FLICKERING_WARD: &str = "Enchant creature\n\
As this Aura enters, choose a color.\n\
Enchanted creature has protection from the chosen color. This effect doesn't remove this Aura.\n\
{W}: Return this Aura to its owner's hand.";

#[test]
fn flickering_ward_parses_protection_source_exemption() {
    let parsed = parse_oracle_text(
        FLICKERING_WARD,
        "Flickering Ward",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let prot = parsed
        .statics
        .iter()
        .find(|s| {
            s.protection_does_not_remove == Some(ProtectionDoesNotRemove::Source)
                && s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Protection(ProtectionTarget::ChosenColor),
                        }
                    )
                })
        })
        .expect("Flickering Ward must carry Protection(ChosenColor) + Source exemption");
    assert_eq!(
        prot.protection_does_not_remove,
        Some(ProtectionDoesNotRemove::Source)
    );
}

#[test]
fn flickering_ward_stays_attached_after_choosing_own_color() {
    let parsed = parse_oracle_text(
        FLICKERING_WARD,
        "Flickering Ward",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let prot_static = parsed
        .statics
        .iter()
        .find(|s| s.protection_does_not_remove.is_some())
        .cloned()
        .expect("protection static with exemption");

    let mut state = GameState::new_two_player(42);
    let creature = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];

    let aura = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Flickering Ward".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&aura).unwrap();
        obj.card_types.core_types = vec![CoreType::Enchantment];
        obj.base_card_types = obj.card_types.clone();
        obj.card_types.subtypes.push("Aura".to_string());
        obj.base_card_types.subtypes.push("Aura".to_string());
        obj.color.push(ManaColor::White);
        obj.attached_to = Some(creature.into());
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::White));
        obj.static_definitions.push(prot_static.clone());
        let base = std::sync::Arc::make_mut(&mut obj.base_static_definitions);
        base.push(prot_static);
    }
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .attachments
        .push(aura);

    state.layers_dirty.mark_full();
    evaluate_layers(&mut state);

    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(
        state.battlefield.contains(&aura),
        "CR 702.16n: Aura must stay on the battlefield after choosing white (Discord #6499)"
    );
    assert_eq!(
        state.objects.get(&aura).and_then(|o| o.attached_to),
        Some(creature.into()),
        "Aura must remain attached to the enchanted creature"
    );
}

#[test]
fn printed_protection_still_removes_white_aura_without_rider() {
    // Sanity: ordinary Pacifism on a host with printed protection from white
    // is still removed — exemptions are per-grant, not global.
    let mut state = GameState::new_two_player(42);
    let creature = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_keywords
            .push(Keyword::Protection(ProtectionTarget::Color(
                ManaColor::White,
            )));
        obj.keywords = obj.base_keywords.clone();
    }
    let aura = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Pacifism".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&aura).unwrap();
        obj.card_types.core_types = vec![CoreType::Enchantment];
        obj.card_types.subtypes.push("Aura".to_string());
        obj.color.push(ManaColor::White);
        obj.attached_to = Some(creature.into());
    }
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .attachments
        .push(aura);

    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(
        !state.battlefield.contains(&aura),
        "printed protection without a 702.16n rider still removes white Auras"
    );
    assert!(
        state.players[1].graveyard.contains(&aura),
        "illegal Aura must go to its owner's graveyard (CR 704.5m)"
    );
}
