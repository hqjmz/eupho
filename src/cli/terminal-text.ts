const TERMINAL_CONTROLS = /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/gu;

/** Render one bounded terminal line from untrusted GitHub text. */
export function terminalText(value: string, maximumLength = 300): string {
  const sanitized = value.replace(TERMINAL_CONTROLS, (character) =>
    character === '\n' || character === '\r' || character === '\t' ? ' ' : '�',
  );
  const compact = sanitized.replace(/ {2,}/gu, ' ').trim();
  return compact.length <= maximumLength
    ? compact
    : `${compact.slice(0, Math.max(0, maximumLength - 1))}…`;
}
