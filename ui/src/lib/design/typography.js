/* Design token typography values as JS constants. */

export const T = {
  fontSans: "'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
  fontMono: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",

  sizes: {
    display1: '2.5rem',
    display2: '2rem',
    display3: '1.5rem',
    heading1: '1.25rem',
    heading2: '1.125rem',
    body:     '1rem',
    small:    '0.875rem',
    mono:     '0.875rem',
    monoSm:   '0.8125rem',
  },

  leading: { display: 1.15, body: 1.5 },
  tracking: { display: '-0.02em', body: 'normal', mono: '0em' },
};
