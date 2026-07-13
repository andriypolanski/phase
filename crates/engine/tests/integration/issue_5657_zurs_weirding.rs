//! Runtime pipeline regression for issue #5657 — Zur's Weirding.
//!
//! Oracle text:
//!   If a player would draw a card, they reveal it instead. Then any other
//!   player may pay 2 life. If a player does, put that card into its owner's
//!   graveyard. Otherwise, that player draws a card.
//!
//! These tests drive the real Draw replacement pipeline via `DebugAction::DrawCards`
//! and the `OpponentMayChoice` fan-out — not parser helpers or direct fan-out
//! branches. P0 controls Zur's Weirding; P1 (a non-controller) is the drawing
//! player so the discriminating assertions exercise affected-player-relative
//! exclusion and referent binding.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn scenario_with_zurs_weirding(
    db: &CardDatabase,
    drawing_player_library_top_to_bottom: &[&str],
) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Zur's Weirding", Zone::Battlefield, db);
    for name in drawing_player_library_top_to_bottom.iter() {
        scenario.add_real_card(P1, name, Zone::Library, db);
    }
    for _ in 0..5 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;
    runner
}

fn issue_single_draw(runner: &mut engine::game::scenario::GameRunner, player: PlayerId) {
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: player,
            count: 1,
        }))
        .expect("debug draw must succeed");
}

fn hand_card_names(state: &engine::types::game_state::GameState, player: PlayerId) -> Vec<String> {
    state.players[player.0 as usize]
        .hand
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect()
}

fn graveyard_card_names(
    state: &engine::types::game_state::GameState,
    player: PlayerId,
) -> Vec<String> {
    state.players[player.0 as usize]
        .graveyard
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect()
}

fn player_life(state: &engine::types::game_state::GameState, player: PlayerId) -> i32 {
    state.players[player.0 as usize].life
}

/// Drive the post-reveal opponent-may prompt and finish resolution.
fn drive_opponent_may_and_finish(runner: &mut engine::game::scenario::GameRunner, accept: bool) {
    let mut saw_opponent_may = false;
    for _ in 0..120 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                saw_opponent_may = true;
                assert_ne!(
                    player, P1,
                    "the drawing player must never be offered the opponent-may choice; got {player:?}"
                );
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("opponent-may decision must succeed");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                if saw_opponent_may {
                    return;
                }
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    runner.advance_until_stack_empty();
                    if runner.state().stack.is_empty() {
                        break;
                    }
                }
            }
        }
    }
    runner.advance_until_stack_empty();
}

/// CR 614.6 + CR 608.2d: When a non-controller draws, another player may accept,
/// pay 2 life, and bin the revealed card into its owner's graveyard.
#[test]
fn zurs_weirding_accept_pays_life_and_bins_revealed_card() {
    let Some(db) = load_db() else {
        return;
    };

    let mut runner = scenario_with_zurs_weirding(db, &["Grizzly Bears", "Forest", "Plains"]);
    let p0_life_before = player_life(runner.state(), P0);
    let p1_hand_before = hand_card_names(runner.state(), P1).len();

    issue_single_draw(&mut runner, P1);
    drive_opponent_may_and_finish(&mut runner, true);

    assert!(
        graveyard_card_names(runner.state(), P1).contains(&"Grizzly Bears".to_string()),
        "accepting must put the revealed top card into its owner's graveyard; \
         graveyard={:?}",
        graveyard_card_names(runner.state(), P1)
    );
    assert!(
        !hand_card_names(runner.state(), P1).contains(&"Grizzly Bears".to_string()),
        "the binned card must not enter the drawing player's hand"
    );
    assert_eq!(
        hand_card_names(runner.state(), P1).len(),
        p1_hand_before,
        "accept branch must not draw the revealed card"
    );
    assert_eq!(
        player_life(runner.state(), P0),
        p0_life_before - 2,
        "the accepting player must pay 2 life"
    );
}

/// CR 614.6 + CR 608.2d: When every other player declines, the drawing player
/// draws the same revealed card.
#[test]
fn zurs_weirding_decline_leaves_drawing_player_with_revealed_card() {
    let Some(db) = load_db() else {
        return;
    };

    let mut runner = scenario_with_zurs_weirding(db, &["Grizzly Bears", "Forest", "Plains"]);
    let p0_life_before = player_life(runner.state(), P0);
    let p1_hand_before = hand_card_names(runner.state(), P1).len();

    issue_single_draw(&mut runner, P1);
    drive_opponent_may_and_finish(&mut runner, false);

    let p1_hand_after = hand_card_names(runner.state(), P1);
    assert_eq!(
        p1_hand_after.len(),
        p1_hand_before + 1,
        "decline must let the drawing player draw the revealed card; hand={p1_hand_after:?}"
    );
    assert!(
        p1_hand_after.contains(&"Grizzly Bears".to_string()),
        "the revealed top card must be drawn on decline; hand={p1_hand_after:?}"
    );
    assert!(
        !graveyard_card_names(runner.state(), P1).contains(&"Grizzly Bears".to_string()),
        "decline must not bin the revealed card"
    );
    assert_eq!(
        player_life(runner.state(), P0),
        p0_life_before,
        "declining must not cost life"
    );
}

/// CR 608.2d + CR 101.4: The drawing player is excluded from the APNAP fan-out.
#[test]
fn zurs_weirding_drawing_player_never_offered_opponent_may() {
    let Some(db) = load_db() else {
        return;
    };

    let mut runner = scenario_with_zurs_weirding(db, &["Grizzly Bears", "Forest"]);
    issue_single_draw(&mut runner, P1);

    let mut saw_opponent_may_for_p0 = false;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                assert_ne!(
                    player, P1,
                    "P1 (drawing player) must never be prompted"
                );
                if player == P0 {
                    saw_opponent_may_for_p0 = true;
                }
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline to finish");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                let _ = runner.act(GameAction::PassPriority);
                runner.advance_until_stack_empty();
            }
        }
    }

    assert!(
        saw_opponent_may_for_p0,
        "P0 must be offered the opponent-may choice when P1 draws"
    );
}
