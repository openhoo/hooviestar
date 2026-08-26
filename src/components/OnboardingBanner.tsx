import { memo } from "react";

interface OnboardingBannerProps {
  onStart: () => void;
  onDismiss: () => void;
}

function OnboardingBannerImpl({ onStart, onDismiss }: OnboardingBannerProps) {
  return (
    <div className="onboarding-banner" role="region" aria-label="Ersteinrichtung">
      <div><strong>Spiel, Video und Beides sind vorbereitet.</strong><span>Füge zuerst ein Fenster oder einen Monitor und danach ein Medium hinzu.</span></div>
      <button onClick={onStart}>Einrichtung starten</button>
      <button onClick={onDismiss}>Später</button>
    </div>
  );
}

export const OnboardingBanner = memo(OnboardingBannerImpl);
