// CX-81 B6: 12-case table-driven test for `firstNameToPediaLabel`.
//
// Matches the helper-rule table in `CX81_B6_ASSISTANT_IDENTITY_BRIEF_v2.md`
// §"Edge case helper". The same 12 cases drive the Python sibling test
// (`cm044/compiler/identity_label_test.py`) and the Swift sibling test
// (`cm031/CM031/Tests/Brand/FirstNameToPediaLabelTests.swift`).
//
// Test names use synthetic identities only (Alice / Bob / Chris / James /
// Connor / Jean-Paul / Mary Beth / O'Brien / Samantha / Andy) per the
// "no real-person test data" rule in `feedback_synthetic_fixtures_no_real_data_default`.

import { describe, expect, it } from 'vitest';
import { firstNameToPediaLabel } from './firstNameToPediaLabel';

describe('firstNameToPediaLabel', () => {
  const cases: { input: string | null | undefined; expected: string; note: string }[] = [
    { input: null, expected: 'Personal wiki', note: 'null falls back to "Personal wiki"' },
    { input: '', expected: 'Personal wiki', note: 'empty string falls back' },
    { input: '   ', expected: 'Personal wiki', note: 'whitespace-only falls back' },
    { input: 'Andy', expected: 'Andypedia', note: 'basic capitalised name' },
    { input: 'Alice', expected: 'Alicepedia', note: 'another basic name' },
    { input: 'Connor', expected: 'Connorpedia', note: 'name ending in r' },
    { input: 'James', expected: 'Jamespedia', note: 'name ending in s -- NO possessive apostrophe' },
    { input: 'Chris', expected: 'Chrispedia', note: 'another name ending in s' },
    { input: 'Jean-Paul', expected: 'Jeanpedia', note: 'hyphenated -- part before first hyphen' },
    { input: 'Mary Beth', expected: 'Marypedia', note: 'two-word -- first word only' },
    { input: "O'Brien", expected: 'OBrienpedia', note: 'apostrophe stripped, casing preserved' },
    { input: 'Samantha', expected: 'Samanthapedia', note: 'longer name, baseline case' },
  ];

  for (const { input, expected, note } of cases) {
    it(`${JSON.stringify(input)} → ${JSON.stringify(expected)} (${note})`, () => {
      expect(firstNameToPediaLabel(input)).toBe(expected);
    });
  }

  describe('additional edge cases', () => {
    it('handles undefined the same as null', () => {
      expect(firstNameToPediaLabel(undefined)).toBe('Personal wiki');
    });

    it('handles smart-quote apostrophe (U+2019) the same as straight apostrophe', () => {
      // U+2019 RIGHT SINGLE QUOTATION MARK is what iOS keyboards emit.
      // Without explicit handling, the wiki label would render as
      // "O’Brienpedia" which is brand-spec wrong.
      expect(firstNameToPediaLabel('O’Brien')).toBe('OBrienpedia');
    });

    it('handles single hyphen input by falling back', () => {
      // Defensive: input that collapses to empty after transformation falls
      // back to "Personal wiki" rather than rendering bare "pedia".
      expect(firstNameToPediaLabel('-')).toBe('Personal wiki');
    });

    it('handles single apostrophe input by falling back', () => {
      expect(firstNameToPediaLabel("'")).toBe('Personal wiki');
    });

    it('preserves lowercase first letter', () => {
      // Customers occasionally lower-case their wizard input. We do NOT
      // re-case it: their choice is preserved.
      expect(firstNameToPediaLabel('andy')).toBe('andypedia');
    });

    it('preserves all-caps casing', () => {
      expect(firstNameToPediaLabel('ANDY')).toBe('ANDYpedia');
    });

    it('trims leading and trailing whitespace before slicing', () => {
      expect(firstNameToPediaLabel('  Andy  ')).toBe('Andypedia');
    });
  });
});
