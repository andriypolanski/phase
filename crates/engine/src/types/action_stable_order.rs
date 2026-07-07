// @generated-style: maintained by action enum structure; update when GameAction grows.
//
// Issue #4878: allocation-free total order for deterministic AI / legal-action
// sorting. Every payload field type used below derives `Ord`, so payload
// comparison reduces to `cmp_val` (a thin `Ord::cmp` wrapper) chained with
// `then_with`. The single exception is `GameAction::Debug`, whose payload
// (`DebugAction`) transitively contains non-`Ord` types (`Keyword`,
// `TokenCharacteristics`); it is a cold path (debug actions are never in
// `legal_actions()`) handled by the exhaustive `cmp_debug_action`. No `Debug`
// string formatting is used for ordering.
use std::cmp::Ordering;

use super::ability::LibraryPosition;
use super::actions::{DebugAction, DebugTokenRequest, GameAction, GameActionKind};

/// Total, allocation-free order over `GameAction`: variant discriminant first
/// (`GameActionKind`, declaration order), then payload fields.
pub fn cmp_game_actions(a: &GameAction, b: &GameAction) -> Ordering {
    GameActionKind::from(a)
        .cmp(&GameActionKind::from(b))
        .then_with(|| cmp_payload(a, b))
}

/// Thin `Ord::cmp` wrapper. Works uniformly for scalars, `Option<T>`, `Vec<T>`,
/// and tuples because each such type is `Ord` when its elements are.
fn cmp_val<T: Ord>(a: &T, b: &T) -> Ordering {
    a.cmp(b)
}

/// Compares payloads of two actions. `cmp_game_actions` only reaches this after
/// the discriminants compared `Equal`, so in practice `a` and `b` are always the
/// same variant; the trailing `_` arm is unreachable but required for
/// exhaustiveness and returns `Equal` (a safe total-order identity).
fn cmp_payload(a: &GameAction, b: &GameAction) -> Ordering {
    match (a, b) {
        (GameAction::PassPriority, GameAction::PassPriority) => Ordering::Equal,
        (
            GameAction::PlayLand {
                object_id: a0,
                card_id: a1,
            },
            GameAction::PlayLand {
                object_id: b0,
                card_id: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::CastSpell {
                object_id: a0,
                card_id: a1,
                targets: a2,
                payment_mode: a3,
            },
            GameAction::CastSpell {
                object_id: b0,
                card_id: b1,
                targets: b2,
                payment_mode: b3,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2))
            .then_with(|| cmp_val(a3, b3)),
        (
            GameAction::Foretell {
                object_id: a0,
                card_id: a1,
            },
            GameAction::Foretell {
                object_id: b0,
                card_id: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::ActivateAbility {
                source_id: a0,
                ability_index: a1,
            },
            GameAction::ActivateAbility {
                source_id: b0,
                ability_index: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::DeclareAttackers {
                attacks: a0,
                bands: a1,
            },
            GameAction::DeclareAttackers {
                attacks: b0,
                bands: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::DeclareBlockers { assignments: a0 },
            GameAction::DeclareBlockers { assignments: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseUntap {
                object_id: a0,
                untap: a1,
            },
            GameAction::ChooseUntap {
                object_id: b0,
                untap: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (GameAction::ChooseExert { exert: a0 }, GameAction::ChooseExert { exert: b0 }) => {
            cmp_val(a0, b0)
        }
        (GameAction::ChooseEnlist { target: a0 }, GameAction::ChooseEnlist { target: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::ChooseClashOpponent { opponent: a0 },
            GameAction::ChooseClashOpponent { opponent: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseAssistPlayer { player: a0 },
            GameAction::ChooseAssistPlayer { player: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::CommitAssistPayment { generic: a0 },
            GameAction::CommitAssistPayment { generic: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::MulliganDecision { choice: a0 },
            GameAction::MulliganDecision { choice: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::ReorderHand { order: a0 }, GameAction::ReorderHand { order: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::TapLandForMana { object_id: a0 },
            GameAction::TapLandForMana { object_id: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::UntapLandForMana { object_id: a0 },
            GameAction::UntapLandForMana { object_id: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::SpendPoolMana { pip_id: a0 }, GameAction::SpendPoolMana { pip_id: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::UnspendPoolMana { pip_id: a0 },
            GameAction::UnspendPoolMana { pip_id: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::SelectCards { cards: a0 }, GameAction::SelectCards { cards: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::ChooseRemoveCounterCostDistribution { distribution: a0 },
            GameAction::ChooseRemoveCounterCostDistribution { distribution: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::SelectCoinFlips { keep_indices: a0 },
            GameAction::SelectCoinFlips { keep_indices: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseOutsideGameCards { selections: a0 },
            GameAction::ChooseOutsideGameCards { selections: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::SelectTargets { targets: a0 }, GameAction::SelectTargets { targets: b0 }) => {
            cmp_val(a0, b0)
        }
        (GameAction::ChooseTarget { target: a0 }, GameAction::ChooseTarget { target: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::ChooseReplacement { index: a0 },
            GameAction::ChooseReplacement { index: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::OrderTriggers { order: a0 }, GameAction::OrderTriggers { order: b0 }) => {
            cmp_val(a0, b0)
        }
        (GameAction::CancelCast, GameAction::CancelCast) => Ordering::Equal,
        (
            GameAction::Equip {
                equipment_id: a0,
                target_id: a1,
            },
            GameAction::Equip {
                equipment_id: b0,
                target_id: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::CrewVehicle {
                vehicle_id: a0,
                creature_ids: a1,
            },
            GameAction::CrewVehicle {
                vehicle_id: b0,
                creature_ids: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::ActivateStation {
                spacecraft_id: a0,
                creature_id: a1,
            },
            GameAction::ActivateStation {
                spacecraft_id: b0,
                creature_id: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::SaddleMount {
                mount_id: a0,
                creature_ids: a1,
            },
            GameAction::SaddleMount {
                mount_id: b0,
                creature_ids: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (GameAction::Transform { object_id: a0 }, GameAction::Transform { object_id: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::PlayFaceDown {
                object_id: a0,
                card_id: a1,
            },
            GameAction::PlayFaceDown {
                object_id: b0,
                card_id: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (GameAction::TurnFaceUp { object_id: a0 }, GameAction::TurnFaceUp { object_id: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::SubmitSideboard {
                main: a0,
                sideboard: a1,
            },
            GameAction::SubmitSideboard {
                main: b0,
                sideboard: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::ChoosePlayDraw { play_first: a0 },
            GameAction::ChoosePlayDraw { play_first: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::ChooseOption { choice: a0 }, GameAction::ChooseOption { choice: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::SubmitVoteCandidate {
                candidate_index: a0,
            },
            GameAction::SubmitVoteCandidate {
                candidate_index: b0,
            },
        ) => cmp_val(a0, b0),
        (
            GameAction::SubmitSpellbookDraft { card: a0 },
            GameAction::SubmitSpellbookDraft { card: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::SubmitPilePartition { pile_a: a0 },
            GameAction::SubmitPilePartition { pile_a: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::ChoosePile { pile: a0 }, GameAction::ChoosePile { pile: b0 }) => {
            cmp_val(a0, b0)
        }
        (GameAction::ChooseBranch { index: a0 }, GameAction::ChooseBranch { index: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::ChooseDamageSource { source: a0 },
            GameAction::ChooseDamageSource { source: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::SelectModes { indices: a0 }, GameAction::SelectModes { indices: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::DecideOptionalCost { pay: a0 },
            GameAction::DecideOptionalCost { pay: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseAdventureFace { creature: a0 },
            GameAction::ChooseAdventureFace { creature: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseModalFace { back_face: a0 },
            GameAction::ChooseModalFace { back_face: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseAlternativeCast { choice: a0 },
            GameAction::ChooseAlternativeCast { choice: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseCastingVariant { index: a0 },
            GameAction::ChooseCastingVariant { index: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::KeepAllCopyTargets, GameAction::KeepAllCopyTargets) => Ordering::Equal,
        (
            GameAction::ChoosePermanentTypeSlot { slot: a0 },
            GameAction::ChoosePermanentTypeSlot { slot: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id: a0,
                creature_to_return: a1,
            },
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id: b0,
                creature_to_return: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::CastSpellAsSneak {
                hand_object: a0,
                card_id: a1,
                creature_to_return: a2,
                payment_mode: a3,
            },
            GameAction::CastSpellAsSneak {
                hand_object: b0,
                card_id: b1,
                creature_to_return: b2,
                payment_mode: b3,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2))
            .then_with(|| cmp_val(a3, b3)),
        (
            GameAction::CastSpellAsWebSlinging {
                hand_object: a0,
                card_id: a1,
                creature_to_return: a2,
                payment_mode: a3,
            },
            GameAction::CastSpellAsWebSlinging {
                hand_object: b0,
                card_id: b1,
                creature_to_return: b2,
                payment_mode: b3,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2))
            .then_with(|| cmp_val(a3, b3)),
        (
            GameAction::CastSpellForFree {
                object_id: a0,
                card_id: a1,
                source_id: a2,
                payment_mode: a3,
            },
            GameAction::CastSpellForFree {
                object_id: b0,
                card_id: b1,
                source_id: b2,
                payment_mode: b3,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2))
            .then_with(|| cmp_val(a3, b3)),
        (
            GameAction::CastSpellAsMiracle {
                object_id: a0,
                card_id: a1,
                payment_mode: a2,
            },
            GameAction::CastSpellAsMiracle {
                object_id: b0,
                card_id: b1,
                payment_mode: b2,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2)),
        (
            GameAction::CastSpellAsMadness {
                object_id: a0,
                card_id: a1,
                payment_mode: a2,
            },
            GameAction::CastSpellAsMadness {
                object_id: b0,
                card_id: b1,
                payment_mode: b2,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2)),
        (
            GameAction::DecideOptionalEffect { accept: a0 },
            GameAction::DecideOptionalEffect { accept: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::RespondToSpliceOffer { card: a0 },
            GameAction::RespondToSpliceOffer { card: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::DecideOptionalEffectAndRemember { choice: a0 },
            GameAction::DecideOptionalEffectAndRemember { choice: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::PayUnlessCost { pay: a0 }, GameAction::PayUnlessCost { pay: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::ChooseUnlessCostBranch { choice: a0 },
            GameAction::ChooseUnlessCostBranch { choice: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseActivationCostBranch { index: a0 },
            GameAction::ChooseActivationCostBranch { index: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::PayCombatTax { accept: a0 }, GameAction::PayCombatTax { accept: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::ChooseRingBearer { target: a0 },
            GameAction::ChooseRingBearer { target: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::ChoosePair { partner: a0 }, GameAction::ChoosePair { partner: b0 }) => {
            cmp_val(a0, b0)
        }
        (GameAction::ChooseDungeon { dungeon: a0 }, GameAction::ChooseDungeon { dungeon: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::ChooseDungeonRoom { room_index: a0 },
            GameAction::ChooseDungeonRoom { room_index: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::UnlockRoomDoor {
                object_id: a0,
                door: a1,
            },
            GameAction::UnlockRoomDoor {
                object_id: b0,
                door: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (GameAction::RollPlanarDie, GameAction::RollPlanarDie) => Ordering::Equal,
        (
            GameAction::ChooseRoomDoor {
                object_id: a0,
                op: a1,
                door: a2,
            },
            GameAction::ChooseRoomDoor {
                object_id: b0,
                op: b1,
                door: b2,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2)),
        (
            GameAction::TapForConvoke {
                object_id: a0,
                mana_type: a1,
            },
            GameAction::TapForConvoke {
                object_id: b0,
                mana_type: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::HarmonizeTap { creature_id: a0 },
            GameAction::HarmonizeTap { creature_id: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::DeclareCompanion { card_index: a0 },
            GameAction::DeclareCompanion { card_index: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::CompanionToHand, GameAction::CompanionToHand) => Ordering::Equal,
        (GameAction::DiscoverChoice { choice: a0 }, GameAction::DiscoverChoice { choice: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::GraveyardPaidCastChoice { choice: a0 },
            GameAction::GraveyardPaidCastChoice { choice: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::CascadeChoice { choice: a0 }, GameAction::CascadeChoice { choice: b0 }) => {
            cmp_val(a0, b0)
        }
        (GameAction::RippleChoice { choice: a0 }, GameAction::RippleChoice { choice: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::FreeCastWindowChoice { selection: a0 },
            GameAction::FreeCastWindowChoice { selection: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::ChooseTopOrBottom { top: a0 }, GameAction::ChooseTopOrBottom { top: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::ChooseMutateMergeSide { side: a0 },
            GameAction::ChooseMutateMergeSide { side: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::CipherEncode { creature: a0 }, GameAction::CipherEncode { creature: b0 }) => {
            cmp_val(a0, b0)
        }
        (GameAction::ChooseLegend { keep: a0 }, GameAction::ChooseLegend { keep: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::ChooseBattleProtector { protector: a0 },
            GameAction::ChooseBattleProtector { protector: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::SetAutoPass { mode: a0 }, GameAction::SetAutoPass { mode: b0 }) => {
            cmp_val(a0, b0)
        }
        (GameAction::CancelAutoPass, GameAction::CancelAutoPass) => Ordering::Equal,
        (GameAction::SetPhaseStops { stops: a0 }, GameAction::SetPhaseStops { stops: b0 }) => {
            cmp_val(a0, b0)
        }
        (GameAction::SetPriorityYield { op: a0 }, GameAction::SetPriorityYield { op: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::AssignCombatDamage {
                mode: a0,
                assignments: a1,
                trample_damage: a2,
                controller_damage: a3,
            },
            GameAction::AssignCombatDamage {
                mode: b0,
                assignments: b1,
                trample_damage: b2,
                controller_damage: b3,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2))
            .then_with(|| cmp_val(a3, b3)),
        (
            GameAction::AssignBlockerDamage { assignments: a0 },
            GameAction::AssignBlockerDamage { assignments: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::DistributeAmong { distribution: a0 },
            GameAction::DistributeAmong { distribution: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseCounterMoveDistribution { selections: a0 },
            GameAction::ChooseCounterMoveDistribution { selections: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseCountersToRemove { selections: a0 },
            GameAction::ChooseCountersToRemove { selections: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::SubmitPayAmount { amount: a0 },
            GameAction::SubmitPayAmount { amount: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::RetargetSpell { new_targets: a0 },
            GameAction::RetargetSpell { new_targets: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::LearnDecision { choice: a0 }, GameAction::LearnDecision { choice: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            GameAction::SelectCategoryPermanents { choices: a0 },
            GameAction::SelectCategoryPermanents { choices: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseKeptCreatures { kept: a0 },
            GameAction::ChooseKeptCreatures { kept: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::ChooseX { value: a0 }, GameAction::ChooseX { value: b0 }) => cmp_val(a0, b0),
        (
            GameAction::SubmitPhyrexianChoices { choices: a0 },
            GameAction::SubmitPhyrexianChoices { choices: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseManaColor {
                choice: a0,
                count: a1,
            },
            GameAction::ChooseManaColor {
                choice: b0,
                count: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            GameAction::PayManaAbilityMana { payment: a0 },
            GameAction::PayManaAbilityMana { payment: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::CastPreparedCopy { source: a0 },
            GameAction::CastPreparedCopy { source: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::ChooseSpecializeColor { color: a0 },
            GameAction::ChooseSpecializeColor { color: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::CastParadigmCopy { source: a0 },
            GameAction::CastParadigmCopy { source: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::PassParadigmOffer, GameAction::PassParadigmOffer) => Ordering::Equal,
        (GameAction::Debug(a0), GameAction::Debug(b0)) => cmp_debug_action(a0, b0),
        (
            GameAction::GrantDebugPermission { player_id: a0 },
            GameAction::GrantDebugPermission { player_id: b0 },
        ) => cmp_val(a0, b0),
        (
            GameAction::RevokeDebugPermission { player_id: a0 },
            GameAction::RevokeDebugPermission { player_id: b0 },
        ) => cmp_val(a0, b0),
        (GameAction::Concede { player_id: a0 }, GameAction::Concede { player_id: b0 }) => {
            cmp_val(a0, b0)
        }
        // Unreachable: `cmp_game_actions` only calls this after the discriminants
        // compared `Equal`, which implies the same variant. `Equal` is the safe
        // total-order identity for the (never-taken) mismatched-variant case.
        _ => Ordering::Equal,
    }
}

/// Cold-path order over `DebugAction`. Debug actions never appear in
/// `legal_actions()` / AI candidate scoring, so this only needs to be total and
/// deterministic. It orders by variant (declaration order via
/// [`debug_action_rank`]) then by the `Ord`-comparable payload fields; `Keyword`
/// (not `Ord`) is compared through its `KeywordKind` discriminant, and
/// `TokenCharacteristics` through its scalar/`Ord` fields.
pub fn cmp_debug_action(a: &DebugAction, b: &DebugAction) -> Ordering {
    debug_action_rank(a)
        .cmp(&debug_action_rank(b))
        .then_with(|| cmp_debug_action_payload(a, b))
}

/// Declaration-order rank for a `DebugAction` variant. Exhaustive: adding a new
/// variant is a compile error here, forcing the ordering to be extended.
fn debug_action_rank(a: &DebugAction) -> u16 {
    match a {
        DebugAction::MoveToZone { .. } => 0,
        DebugAction::CreateCard { .. } => 1,
        DebugAction::RemoveObject { .. } => 2,
        DebugAction::Sacrifice { .. } => 3,
        DebugAction::DrawCards { .. } => 4,
        DebugAction::Mill { .. } => 5,
        DebugAction::Reveal { .. } => 6,
        DebugAction::ShuffleLibrary { .. } => 7,
        DebugAction::Proliferate { .. } => 8,
        DebugAction::SetBasePowerToughness { .. } => 9,
        DebugAction::ModifyCounters { .. } => 10,
        DebugAction::SetTapped { .. } => 11,
        DebugAction::SetPrepared { .. } => 12,
        DebugAction::SetController { .. } => 13,
        DebugAction::SetSummoningSickness { .. } => 14,
        DebugAction::SetFaceState { .. } => 15,
        DebugAction::Attach { .. } => 16,
        DebugAction::Detach { .. } => 17,
        DebugAction::GrantKeyword { .. } => 18,
        DebugAction::RemoveKeyword { .. } => 19,
        DebugAction::SetLife { .. } => 20,
        DebugAction::ModifyPlayerCounters { .. } => 21,
        DebugAction::ModifyEnergy { .. } => 22,
        DebugAction::AddMana { .. } => 23,
        DebugAction::SetInfiniteMana { .. } => 24,
        DebugAction::SetPhase { .. } => 25,
        DebugAction::RunStateBasedActions => 26,
        DebugAction::CreateToken { .. } => 27,
        DebugAction::CreateTokenCopy { .. } => 28,
    }
}

fn cmp_debug_action_payload(a: &DebugAction, b: &DebugAction) -> Ordering {
    match (a, b) {
        (
            DebugAction::MoveToZone {
                object_id: a0,
                to_zone: a1,
                library_position: a2,
                simulate: a3,
            },
            DebugAction::MoveToZone {
                object_id: b0,
                to_zone: b1,
                library_position: b2,
                simulate: b3,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_opt_library_position(a2, b2))
            .then_with(|| cmp_val(a3, b3)),
        (
            DebugAction::CreateCard {
                card_name: a0,
                owner: a1,
                zone: a2,
                attach_to: a3,
                run_etb: a4,
            },
            DebugAction::CreateCard {
                card_name: b0,
                owner: b1,
                zone: b2,
                attach_to: b3,
                run_etb: b4,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2))
            .then_with(|| cmp_val(a3, b3))
            .then_with(|| cmp_val(a4, b4)),
        (
            DebugAction::RemoveObject { object_id: a0 },
            DebugAction::RemoveObject { object_id: b0 },
        ) => cmp_val(a0, b0),
        (DebugAction::Sacrifice { object_id: a0 }, DebugAction::Sacrifice { object_id: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            DebugAction::DrawCards {
                player_id: a0,
                count: a1,
            },
            DebugAction::DrawCards {
                player_id: b0,
                count: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::Mill {
                player_id: a0,
                count: a1,
            },
            DebugAction::Mill {
                player_id: b0,
                count: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::Reveal {
                player_id: a0,
                count: a1,
            },
            DebugAction::Reveal {
                player_id: b0,
                count: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::ShuffleLibrary { player_id: a0 },
            DebugAction::ShuffleLibrary { player_id: b0 },
        ) => cmp_val(a0, b0),
        (
            DebugAction::Proliferate { player_id: a0 },
            DebugAction::Proliferate { player_id: b0 },
        ) => cmp_val(a0, b0),
        (
            DebugAction::SetBasePowerToughness {
                object_id: a0,
                power: a1,
                toughness: a2,
            },
            DebugAction::SetBasePowerToughness {
                object_id: b0,
                power: b1,
                toughness: b2,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2)),
        (
            DebugAction::ModifyCounters {
                object_id: a0,
                counter_type: a1,
                delta: a2,
            },
            DebugAction::ModifyCounters {
                object_id: b0,
                counter_type: b1,
                delta: b2,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2)),
        (
            DebugAction::SetTapped {
                object_id: a0,
                tapped: a1,
            },
            DebugAction::SetTapped {
                object_id: b0,
                tapped: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::SetPrepared {
                object_id: a0,
                prepared: a1,
            },
            DebugAction::SetPrepared {
                object_id: b0,
                prepared: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::SetController {
                object_id: a0,
                controller: a1,
            },
            DebugAction::SetController {
                object_id: b0,
                controller: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::SetSummoningSickness {
                object_id: a0,
                sick: a1,
            },
            DebugAction::SetSummoningSickness {
                object_id: b0,
                sick: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::SetFaceState {
                object_id: a0,
                face_down: a1,
                transformed: a2,
                flipped: a3,
            },
            DebugAction::SetFaceState {
                object_id: b0,
                face_down: b1,
                transformed: b2,
                flipped: b3,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2))
            .then_with(|| cmp_val(a3, b3)),
        (
            DebugAction::Attach {
                object_id: a0,
                target: a1,
            },
            DebugAction::Attach {
                object_id: b0,
                target: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (DebugAction::Detach { object_id: a0 }, DebugAction::Detach { object_id: b0 }) => {
            cmp_val(a0, b0)
        }
        (
            DebugAction::GrantKeyword {
                object_id: a0,
                keyword: a1,
            },
            DebugAction::GrantKeyword {
                object_id: b0,
                keyword: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(&a1.kind(), &b1.kind())),
        (
            DebugAction::RemoveKeyword {
                object_id: a0,
                keyword: a1,
            },
            DebugAction::RemoveKeyword {
                object_id: b0,
                keyword: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(&a1.kind(), &b1.kind())),
        (
            DebugAction::SetLife {
                player_id: a0,
                life: a1,
            },
            DebugAction::SetLife {
                player_id: b0,
                life: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::ModifyPlayerCounters {
                player_id: a0,
                counter_kind: a1,
                delta: a2,
            },
            DebugAction::ModifyPlayerCounters {
                player_id: b0,
                counter_kind: b1,
                delta: b2,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(a1, b1))
            .then_with(|| cmp_val(a2, b2)),
        (
            DebugAction::ModifyEnergy {
                player_id: a0,
                delta: a1,
            },
            DebugAction::ModifyEnergy {
                player_id: b0,
                delta: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::AddMana {
                player_id: a0,
                mana: a1,
            },
            DebugAction::AddMana {
                player_id: b0,
                mana: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::SetInfiniteMana {
                player_id: a0,
                enabled: a1,
            },
            DebugAction::SetInfiniteMana {
                player_id: b0,
                enabled: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::SetPhase {
                phase: a0,
                active_player: a1,
            },
            DebugAction::SetPhase {
                phase: b0,
                active_player: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        (DebugAction::RunStateBasedActions, DebugAction::RunStateBasedActions) => Ordering::Equal,
        (
            DebugAction::CreateToken {
                request: a0,
                run_etb: a1,
            },
            DebugAction::CreateToken {
                request: b0,
                run_etb: b1,
            },
        ) => cmp_debug_token_request(a0, b0).then_with(|| cmp_val(a1, b1)),
        (
            DebugAction::CreateTokenCopy {
                source_id: a0,
                owner: a1,
            },
            DebugAction::CreateTokenCopy {
                source_id: b0,
                owner: b1,
            },
        ) => cmp_val(a0, b0).then_with(|| cmp_val(a1, b1)),
        // Unreachable: reached only after `debug_action_rank` compared `Equal`,
        // which implies the same variant.
        _ => Ordering::Equal,
    }
}

/// Cold-path order over `Option<LibraryPosition>`. `LibraryPosition` cannot
/// derive `Ord` because `BeneathTop { depth }` carries a non-`Ord`
/// `QuantityExpr`; two `BeneathTop` values therefore tie-break as `Equal`
/// (sufficient for this cold debug path).
fn cmp_opt_library_position(a: &Option<LibraryPosition>, b: &Option<LibraryPosition>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => cmp_library_position(a, b),
    }
}

fn cmp_library_position(a: &LibraryPosition, b: &LibraryPosition) -> Ordering {
    fn rank(p: &LibraryPosition) -> u8 {
        match p {
            LibraryPosition::Top => 0,
            LibraryPosition::Bottom => 1,
            LibraryPosition::NthFromTop { .. } => 2,
            LibraryPosition::BeneathTop { .. } => 3,
        }
    }
    rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
        (LibraryPosition::NthFromTop { n: a0 }, LibraryPosition::NthFromTop { n: b0 }) => {
            cmp_val(a0, b0)
        }
        // `BeneathTop` carries a non-`Ord` `QuantityExpr`; equal-rank fallback.
        _ => Ordering::Equal,
    })
}

/// Cold-path order over `DebugTokenRequest`. `Preset` sorts before `Custom`;
/// within each, orders by owner then the `Ord`-comparable fields. The
/// non-`Ord` `TokenCharacteristics` is compared only through its
/// `display_name` (sufficient for a deterministic total order on this cold path).
fn cmp_debug_token_request(a: &DebugTokenRequest, b: &DebugTokenRequest) -> Ordering {
    fn rank(r: &DebugTokenRequest) -> u8 {
        match r {
            DebugTokenRequest::Preset { .. } => 0,
            DebugTokenRequest::Custom { .. } => 1,
        }
    }
    rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
        (
            DebugTokenRequest::Preset {
                preset_id: a0,
                owner: a1,
                power_override: a2,
                toughness_override: a3,
                enter_with_counters: a4,
            },
            DebugTokenRequest::Preset {
                preset_id: b0,
                owner: b1,
                power_override: b2,
                toughness_override: b3,
                enter_with_counters: b4,
            },
        ) => cmp_val(a1, b1)
            .then_with(|| cmp_val(a0, b0))
            .then_with(|| cmp_val(a2, b2))
            .then_with(|| cmp_val(a3, b3))
            .then_with(|| cmp_val(a4, b4)),
        (
            DebugTokenRequest::Custom {
                owner: a0,
                characteristics: a1,
                enter_with_counters: a2,
            },
            DebugTokenRequest::Custom {
                owner: b0,
                characteristics: b1,
                enter_with_counters: b2,
            },
        ) => cmp_val(a0, b0)
            .then_with(|| cmp_val(&a1.display_name, &b1.display_name))
            .then_with(|| cmp_val(a2, b2)),
        // Unreachable: reached only after `rank` compared `Equal`.
        _ => Ordering::Equal,
    })
}
