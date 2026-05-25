/**
 * LetterAvatar — Ostler brand mark family per BRAND_SPEC_V1.1 §3.6
 *
 * Two members, never more:
 *
 *   Instance A — "product mark":
 *     Solid oxblood circle, white "O" letter.
 *     Used wherever Ostler-the-product needs to identify itself
 *     (Hub sidebar header, OstlerInstaller welcome, marketing,
 *     menu bar icon basis).
 *
 *   Instance B — "assistant mark":
 *     `--accent-soft` filled circle, 1.5px oxblood ring, ink letter.
 *     Used wherever the customer's chosen assistant needs to
 *     identify itself (Hub assistant card, iOS chat header).
 *     Letter is `{{user_assistant_name | first_letter}}`.
 *
 * Don'ts (V1.1 Don'ts #15): never improvise a third instance.
 * Don'ts (V1.1 Don'ts #10): no sparkle, no robot, no "AI" iconography
 *                           — the letter-avatar is the only assistant mark.
 */

interface LetterAvatarProps {
  /** Which member of the family. */
  instance: 'product' | 'assistant';
  /**
   * The letter to render inside the circle. For 'product', this is
   * conventionally 'O'. For 'assistant', this is the first letter of the
   * customer's chosen assistant name, uppercased.
   */
  letter: string;
  /**
   * Pixel size of the circle. Default 32px (per spec). Hub sidebar uses 28px;
   * the chat surface uses 32px; hero treatments use 64px+.
   */
  size?: number;
  /** Accessible label override. Defaults to the letter. */
  ariaLabel?: string;
}

export default function LetterAvatar({
  instance,
  letter,
  size = 32,
  ariaLabel,
}: LetterAvatarProps) {
  // Letter size scales with the avatar (50% of the circle diameter at the
  // default 32px → 16px; tracks the spec's 32→16 / 28→14 / 64→32 ratios).
  const fontSize = Math.round(size * 0.5);

  return (
    <div
      role="img"
      aria-label={ariaLabel ?? letter}
      className={`letter-avatar letter-avatar--${instance}`}
      style={{ width: size, height: size, fontSize }}
    >
      {/* Always single-character display — strip to first character to enforce
          the family's "letter only, never name" contract. */}
      {letter.slice(0, 1).toUpperCase()}
    </div>
  );
}
