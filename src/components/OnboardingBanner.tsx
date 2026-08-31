import { memo } from "react";

interface OnboardingBannerProps {
  onStart: () => void;
  onDismiss: () => void;
}

function OnboardingBannerImpl({ onStart, onDismiss }: OnboardingBannerProps) {
  return (
    <div className="onboarding-banner" role="region" aria-label="Ersteinrichtung">
      <span className="onboarding-mark" aria-hidden="true">✦</span>
      <div className="onboarding-copy">
        <strong>Dein Studio ist vorbereitet.</strong>
        <span>Füge ein Fenster oder einen Monitor hinzu. Hooviestar ordnet es den Standardszenen zu.</span>
      </div>
      <div className="onboarding-actions">
        <button type="button" onClick={onDismiss}>Später</button>
        <button type="button" className="primary" onClick={onStart}>Quelle hinzufügen</button>
      </div>
    </div>
  );
}

export const OnboardingBanner = memo(OnboardingBannerImpl);
