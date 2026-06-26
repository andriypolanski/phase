//! Server-side turn-clock timer tasks for live multiplayer games.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use engine::game::derived_views::derive_views;
use engine::types::player::PlayerId;
use server_core::game_state_snapshot_wire_guard::{
    guard_state_snapshot_broadcast, StateSnapshotParts,
};
use server_core::protocol::ServerMessage;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::{
    build_spectator_state_update_message, persist_session_async, SharedConnections, SharedGameDb,
    SharedGameSpectators, SharedState,
};

pub type GameTurnClockTasks = Arc<Mutex<HashMap<String, JoinHandle<()>>>>;

pub async fn broadcast_turn_clock_sync(
    game_code: &str,
    msg: ServerMessage,
    connections: &SharedConnections,
    game_spectators: &SharedGameSpectators,
) {
    let conns = connections.lock().await;
    if let Some(players) = conns.get(game_code) {
        for sender in players.values() {
            let _ = sender.send(msg.clone());
        }
    }
    drop(conns);

    let mut specs = game_spectators.lock().await;
    if let Some(spectators) = specs.get_mut(game_code) {
        spectators.retain(|sender| sender.send(msg.clone()).is_ok());
        if spectators.is_empty() {
            specs.remove(game_code);
        }
    }
}

pub async fn reschedule_game_turn_clock(
    state: SharedState,
    connections: SharedConnections,
    game_spectators: SharedGameSpectators,
    game_db: SharedGameDb,
    timer_tasks: GameTurnClockTasks,
    game_code: String,
) {
    let sync_msg = {
        let mut mgr = state.lock().await;
        let Some(session) = mgr.sessions.get_mut(&game_code) else {
            let mut tasks = timer_tasks.lock().await;
            if let Some(handle) = tasks.remove(&game_code) {
                handle.abort();
            }
            return;
        };
        session.refresh_turn_clock();
        session.turn_clock_sync_message()
    };

    let Some(sync_msg) = sync_msg else {
        let mut tasks = timer_tasks.lock().await;
        if let Some(handle) = tasks.remove(&game_code) {
            handle.abort();
        }
        return;
    };

    broadcast_turn_clock_sync(&game_code, sync_msg, &connections, &game_spectators).await;

    let mut tasks = timer_tasks.lock().await;
    if let Some(prev) = tasks.remove(&game_code) {
        prev.abort();
    }

    let handle = tokio::spawn(run_turn_clock_loop(
        state,
        connections,
        game_spectators,
        game_db,
        timer_tasks.clone(),
        game_code.clone(),
    ));
    tasks.insert(game_code, handle);
}

async fn run_turn_clock_loop(
    state: SharedState,
    connections: SharedConnections,
    game_spectators: SharedGameSpectators,
    game_db: SharedGameDb,
    _timer_tasks: GameTurnClockTasks,
    game_code: String,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let (sync_msg, expired, player_count) = {
            let mut mgr = state.lock().await;
            let Some(session) = mgr.sessions.get_mut(&game_code) else {
                return;
            };
            if session.timer_seconds.filter(|s| *s > 0).is_none() {
                return;
            }
            session.refresh_turn_clock();
            let expired = session.turn_clock_expired();
            let sync_msg = session.turn_clock_sync_message();
            let player_count = session.player_count;
            (sync_msg, expired, player_count)
        };

        if let Some(msg) = sync_msg {
            broadcast_turn_clock_sync(&game_code, msg, &connections, &game_spectators).await;
        }

        if !expired {
            continue;
        }

        info!(game = %game_code, "turn clock expired — auto-passing priority");

        let timeout_bundle = {
            let mut mgr = state.lock().await;
            match mgr.handle_turn_clock_timeout(&game_code) {
                Ok(Some(human_result)) => {
                    let ai_results = mgr
                        .sessions
                        .get_mut(&game_code)
                        .map(|s| s.run_ai())
                        .unwrap_or_default();
                    if let Some(session) = mgr.sessions.get(&game_code) {
                        persist_session_async(&game_db, &game_code, session);
                    }
                    Some((human_result, ai_results, player_count))
                }
                Ok(None) => {
                    if let Some(session) = mgr.sessions.get_mut(&game_code) {
                        session.refresh_turn_clock();
                    }
                    None
                }
                Err(e) => {
                    warn!(game = %game_code, error = %e, "turn clock timeout failed");
                    None
                }
            }
        };

        if let Some((human_result, ai_results, player_count)) = timeout_bundle {
            broadcast_timeout_action_bundle(
                &game_code,
                human_result,
                ai_results,
                player_count,
                &connections,
                &game_spectators,
            )
            .await;
        }
    }
}

async fn broadcast_timeout_action_bundle(
    game_code: &str,
    human_result: server_core::session::ActionResult,
    ai_results: Vec<server_core::session::ActionResult>,
    player_count: u8,
    connections: &SharedConnections,
    game_spectators: &SharedGameSpectators,
) {
    let (
        raw_state,
        events,
        legal_actions,
        log_entries,
        auto_pass_rec,
        spell_costs,
        legal_actions_by_object,
    ) = human_result;

    if guard_state_snapshot_broadcast(StateSnapshotParts {
        state: &raw_state,
        events: &events,
        log_entries: &log_entries,
        legal_actions: &legal_actions,
        legal_actions_by_object: &legal_actions_by_object,
        spell_costs: &spell_costs,
    })
    .is_err()
    {
        return;
    }

    let eliminated = raw_state.eliminated_players.clone();
    let filtered_states: Vec<(PlayerId, engine::types::game_state::GameState)> = (0..player_count)
        .map(|i| {
            let pid = PlayerId(i);
            (pid, server_core::filter_state_for_player(&raw_state, pid))
        })
        .collect();

    {
        let conns = connections.lock().await;
        if let Some(players) = conns.get(game_code) {
            for (pid, pstate) in &filtered_states {
                if let Some(s) = players.get(pid) {
                    let actors = raw_state.waiting_for.acting_players();
                    let is_actor = actors.contains(pid);
                    let _ = s.send(ServerMessage::StateUpdate {
                        state: pstate.clone(),
                        events: events.clone(),
                        legal_actions: if ai_results.is_empty() && is_actor {
                            legal_actions.clone()
                        } else {
                            vec![]
                        },
                        auto_pass_recommended: if ai_results.is_empty() && is_actor {
                            auto_pass_rec
                        } else {
                            false
                        },
                        eliminated_players: eliminated.clone(),
                        log_entries: log_entries.clone(),
                        spell_costs: if ai_results.is_empty() && is_actor {
                            spell_costs.clone()
                        } else {
                            Default::default()
                        },
                        legal_actions_by_object: if ai_results.is_empty() && is_actor {
                            legal_actions_by_object.clone()
                        } else {
                            Default::default()
                        },
                        derived: derive_views(pstate, Some(*pid)),
                    });
                }
            }
        }
    }

    if let Ok(spectator_msg) =
        build_spectator_state_update_message(&raw_state, &events, &log_entries)
    {
        let mut specs = game_spectators.lock().await;
        if let Some(spectators) = specs.get_mut(game_code) {
            spectators.retain(|sender| sender.send(spectator_msg.clone()).is_ok());
        }
    }

    for (i, result) in ai_results.iter().enumerate() {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (
            ai_raw_state,
            ai_events,
            ai_legal,
            ai_log_entries,
            ai_auto_pass,
            ai_spell_costs,
            ai_by_object,
        ) = result;
        if guard_state_snapshot_broadcast(StateSnapshotParts {
            state: ai_raw_state,
            events: ai_events,
            log_entries: ai_log_entries,
            legal_actions: ai_legal,
            legal_actions_by_object: ai_by_object,
            spell_costs: ai_spell_costs,
        })
        .is_err()
        {
            continue;
        }
        let is_last = i == ai_results.len() - 1;
        let ai_filtered: Vec<(PlayerId, engine::types::game_state::GameState)> = (0..player_count)
            .map(|j| {
                let pid = PlayerId(j);
                (pid, server_core::filter_state_for_player(ai_raw_state, pid))
            })
            .collect();
        let ai_actors = ai_raw_state.waiting_for.acting_players();
        let conns = connections.lock().await;
        if let Some(players) = conns.get(game_code) {
            for (pid, pstate) in &ai_filtered {
                if let Some(s) = players.get(pid) {
                    let is_actor = ai_actors.contains(pid);
                    let _ = s.send(ServerMessage::StateUpdate {
                        state: pstate.clone(),
                        events: ai_events.clone(),
                        legal_actions: if is_last && is_actor {
                            ai_legal.clone()
                        } else {
                            vec![]
                        },
                        auto_pass_recommended: if is_last && is_actor {
                            *ai_auto_pass
                        } else {
                            false
                        },
                        eliminated_players: eliminated.clone(),
                        log_entries: ai_log_entries.clone(),
                        spell_costs: if is_last && is_actor {
                            ai_spell_costs.clone()
                        } else {
                            Default::default()
                        },
                        legal_actions_by_object: if is_last && is_actor {
                            ai_by_object.clone()
                        } else {
                            Default::default()
                        },
                        derived: derive_views(pstate, Some(*pid)),
                    });
                }
            }
        }
    }
}
