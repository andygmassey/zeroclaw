// CX-81 B6: render the customer-personalised wiki nav label.
//
// Same logic in three languages -- keep them in lock-step:
//   - This module (TS, used by the Tauri Hub sidebar via B7 + Settings)
//   - `cm044/compiler/identity_label.py` (Python, used by the wiki compiler)
//   - `cm031/CM031/Sources/Brand/FirstNameToPediaLabel.swift` (Swift, used by
//     the iOS Companion when it re-templates `tab.wiki`)
//
// Rules (per `BRAND_SPEC_V1.1.md` §5.2):
//   - null / empty / whitespace → "Personal wiki"
//   - multi-word names → first word only (e.g. "Mary Beth" → "Marypedia")
//   - apostrophes stripped, casing preserved (e.g. "O'Brien" → "OBrienpedia")
//   - hyphenated names → part before first hyphen (e.g. "Jean-Paul" → "Jeanpedia")
//   - no possessive apostrophe before "pedia" (e.g. "James" → "Jamespedia", NOT "James'pedia")
//
// Non-symmetric fallback (§8 #2): if the first name is null but the assistant
// name is set, this still returns "Personal wiki" -- it does NOT borrow the
// assistant name. Callers must NOT pass the assistant name as a substitute.

/**
 * Render the wiki-nav label for the customer's chosen first name.
 *
 * @param firstName - the customer's first name captured at setup, or null
 * @returns the label to display in the Hub sidebar / iOS tab / Wiki title
 */
export function firstNameToPediaLabel(firstName: string | null | undefined): string {
  if (firstName == null) return 'Personal wiki';
  const trimmed = firstName.trim();
  if (trimmed.length === 0) return 'Personal wiki';

  // First word for multi-word names ("Mary Beth" -> "Mary")
  const firstWord = trimmed.split(/\s+/)[0];

  // Strip apostrophes ("O'Brien" -> "OBrien"). Preserve original casing per spec.
  // U+0027 APOSTROPHE + U+2019 RIGHT SINGLE QUOTATION MARK (smart-quote).
  const stripped = firstWord.replace(/['’]/g, '');

  // Hyphenated: use part before first hyphen ("Jean-Paul" -> "Jean").
  const beforeHyphen = stripped.split('-')[0];

  // Defensive: if all transformations consumed the input (e.g. input was "-"
  // alone or "'"), fall back to the unset label rather than emitting "pedia"
  // by itself, which would look broken.
  if (beforeHyphen.length === 0) return 'Personal wiki';

  return `${beforeHyphen}pedia`;
}
