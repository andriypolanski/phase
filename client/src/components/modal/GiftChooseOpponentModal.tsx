import { useTranslation } from "react-i18next";

import type { GameAction, PlayerId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type GiftChooseOpponentWaitingFor = Extract<WaitingFor, { type: "GiftChooseOpponent" }>;

interface GiftChooseOpponentModalContentProps {
  waitingFor: GiftChooseOpponentWaitingFor;
  seatOrder?: PlayerId[];
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 702.174a: When promising a Gift, the caster chooses which opponent
 * receives the gift before the spell finishes casting.
 */
export function GiftChooseOpponentModalContent({
  waitingFor,
  seatOrder,
  dispatch,
}: GiftChooseOpponentModalContentProps) {
  const { t } = useTranslation("game");
  const candidates = [...waitingFor.data.candidates].sort((a, b) => {
    const aIdx = seatOrder?.indexOf(a) ?? a;
    const bIdx = seatOrder?.indexOf(b) ?? b;
    return aIdx - bIdx;
  });

  return (
    <ChoiceModal
      title={t("giftRecipient.title")}
      subtitle={t("giftRecipient.subtitle")}
      options={candidates.map((opponent) => ({
        id: String(opponent),
        label: getOpponentDisplayName(opponent),
      }))}
      onChoose={(id) => {
        dispatch({
          type: "ChooseGiftRecipient",
          data: { opponent: Number(id) },
        });
      }}
    />
  );
}

export function GiftChooseOpponentModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);

  if (waitingFor?.type !== "GiftChooseOpponent") return null;
  if (!canActForWaitingState) return null;

  return (
    <GiftChooseOpponentModalContent
      waitingFor={waitingFor}
      seatOrder={seatOrder}
      dispatch={dispatch}
    />
  );
}
