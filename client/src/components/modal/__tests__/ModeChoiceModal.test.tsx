import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ModalChoice, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState, buildPendingCast, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { ModeChoiceModal } from "../ModeChoiceModal.tsx";

const dispatchMock = vi.fn();

function singleChoiceModal(): ModalChoice {
  return {
    min_choices: 1,
    max_choices: 1,
    mode_count: 2,
    mode_descriptions: ["You gain 2 life.", "You lose 2 life."],
    allow_repeat_modes: false,
  };
}

function setWaitingFor(waitingFor: WaitingFor) {
  const gameState = buildGameState({
    objects: {},
    priority_player: 0,
    waiting_for: waitingFor,
  });

  useGameStore.setState({
    gameState,
    waitingFor,
    dispatch: dispatchMock,
  });
}

describe("ModeChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    dispatchMock.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
  });

  it("shows a Cancel affordance for an activated modal ability (CR 602.2b) and dispatches CancelCast", () => {
    setWaitingFor({
      type: "AbilityModeChoice",
      data: {
        player: 0,
        modal: singleChoiceModal(),
        source_id: 90,
        mode_abilities: [],
        is_activated: true,
      },
    });

    render(<ModeChoiceModal />);

    // Both mode rows render; single-choice modes auto-dispatch on click.
    expect(screen.getByText("You gain 2 life.")).toBeInTheDocument();
    const cancel = screen.getByRole("button", { name: "Cancel" });
    fireEvent.click(cancel);
    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("hides the Cancel affordance for a triggered modal ability (CR 603.3c)", () => {
    setWaitingFor({
      type: "AbilityModeChoice",
      data: {
        player: 0,
        modal: singleChoiceModal(),
        source_id: 90,
        mode_abilities: [],
        is_activated: false,
      },
    });

    render(<ModeChoiceModal />);

    expect(screen.getByText("You gain 2 life.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
  });

  it("keeps the Cancel affordance for a modal spell (regression guard)", () => {
    setWaitingFor({
      type: "ModeChoice",
      data: {
        player: 0,
        modal: singleChoiceModal(),
        pending_cast: buildPendingCast({ object_id: 50 }),
      },
    });

    render(<ModeChoiceModal />);

    const cancel = screen.getByRole("button", { name: "Cancel" });
    fireEvent.click(cancel);
    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("caps per-mode payment selection using engine max_affordable_selections, not pool size", () => {
    const modal: ModalChoice = {
      min_choices: 0,
      max_choices: 3,
      max_affordable_selections: 1,
      mode_count: 3,
      mode_descriptions: ["Mode A", "Mode B", "Mode C"],
      allow_repeat_modes: false,
      mode_costs: [
        { type: "Cost", generic: 1, shards: ["R"] },
        { type: "Cost", generic: 1, shards: ["R"] },
        { type: "Cost", generic: 1, shards: ["R"] },
      ],
    };
    const gameState = buildGameState({
      objects: {},
      priority_player: 0,
      players: buildPlayers([
        {
          id: 0,
          mana_pool: {
            mana: [
              { color: "Red", source_id: 1, pip_id: 1, snow: false, restrictions: [] },
              { color: "Red", source_id: 2, pip_id: 2, snow: false, restrictions: [] },
              { color: "Red", source_id: 3, pip_id: 3, snow: false, restrictions: [] },
            ],
          },
        },
        1,
      ]),
      waiting_for: {
        type: "AbilityModeChoice",
        data: {
          player: 0,
          modal,
          source_id: 90,
          mode_abilities: [],
          is_activated: false,
        },
      },
    });

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      dispatch: dispatchMock,
    });

    render(<ModeChoiceModal />);

    fireEvent.click(screen.getByText("Mode A"));
    fireEvent.click(screen.getByText("Mode B"));
    expect(screen.getByRole("button", { name: "Confirm (1/1 modes)" })).not.toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Confirm (1/1 modes)" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectModes",
      data: { indices: [0] },
    });
  });

  it("caps repeatable mode-cost selection at modal.max_choices", () => {
    const modal: ModalChoice = {
      min_choices: 0,
      max_choices: 2,
      max_affordable_selections: 3,
      mode_count: 2,
      mode_descriptions: ["Mode A", "Mode B"],
      allow_repeat_modes: true,
      mode_costs: [
        { type: "Cost", generic: 1, shards: [] },
        { type: "Cost", generic: 1, shards: [] },
      ],
    };
    const gameState = buildGameState({
      objects: {},
      priority_player: 0,
      players: buildPlayers([1, 1]),
      waiting_for: {
        type: "AbilityModeChoice",
        data: {
          player: 0,
          modal,
          source_id: 90,
          mode_abilities: [],
          is_activated: false,
        },
      },
    });

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      dispatch: dispatchMock,
    });

    render(<ModeChoiceModal />);

    fireEvent.click(screen.getByText("Mode A"));
    fireEvent.click(screen.getByText("Mode A"));
    fireEvent.click(screen.getByText("Mode B"));
    expect(screen.getByRole("button", { name: "Confirm (2/2 modes)" })).not.toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Confirm (2/2 modes)" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectModes",
      data: { indices: [0, 0] },
    });
  });
});
