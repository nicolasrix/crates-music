/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // Surfaces and foregrounds reference CSS variables so theme swaps
        // (data-theme=light) flow through utility classes for free.
        surface: {
          0: "var(--surface-0)",
          1: "var(--surface-1)",
          2: "var(--surface-2)",
          3: "var(--surface-3)",
        },
        fg: {
          DEFAULT: "var(--fg)",
          strong: "var(--fg-strong)",
          muted: "var(--fg-muted)",
          faint: "var(--fg-faint)",
        },
        accent: {
          DEFAULT: "var(--accent)",
          hover: "var(--accent-hover)",
          press: "var(--accent-press)",
          on: "var(--on-accent)",
        },
        border: {
          subtle: "var(--border-subtle)",
          strong: "var(--border-strong)",
        },
        success: "var(--success)",
        warning: "var(--warning)",
        danger: "var(--danger)",
        info: "var(--info)",
        // Per-page artwork-tinted accent — chrome must NOT use this; only
        // detail-page components (scrubber, now-playing row, hero play disc).
        art: {
          bg: "var(--art-bg)",
          fg: "var(--art-fg)",
          mute: "var(--art-mute)",
          accent: "var(--art-accent)",
        },
      },
      fontFamily: {
        sans: ["var(--font-sans)"],
        display: ["var(--font-display)"],
        mono: ["var(--font-mono)"],
      },
      fontSize: {
        xs:  "var(--text-xs)",
        sm:  "var(--text-sm)",
        base: "var(--text-base)",
        md:  "var(--text-md)",
        lg:  "var(--text-lg)",
        xl:  "var(--text-xl)",
        "2xl": "var(--text-2xl)",
        "3xl": "var(--text-3xl)",
      },
      borderRadius: {
        1: "var(--radius-1)",
        2: "var(--radius-2)",
        3: "var(--radius-3)",
        4: "var(--radius-4)",
      },
      boxShadow: {
        pop: "var(--shadow-pop)",
        overlay: "var(--shadow-overlay)",
      },
      transitionTimingFunction: {
        "out-soft": "var(--ease-out-soft)",
        "in-soft": "var(--ease-in-soft)",
      },
      transitionDuration: {
        1: "var(--dur-1)",
        2: "var(--dur-2)",
        3: "var(--dur-3)",
      },
      spacing: {
        // The 1..9 scale from tokens. Tailwind defaults already cover most of
        // these (1=4px, 2=8px, 4=16px, 6=24px, 8=32px, 12=48px) but exposing
        // them by token name lets us match the design spec verbatim in JSX.
        ds1: "var(--space-1)",
        ds2: "var(--space-2)",
        ds3: "var(--space-3)",
        ds4: "var(--space-4)",
        ds5: "var(--space-5)",
        ds6: "var(--space-6)",
        ds7: "var(--space-7)",
        ds8: "var(--space-8)",
        ds9: "var(--space-9)",
        sidebar: "var(--sidebar-w)",
        player: "var(--player-h)",
        header: "var(--header-h)",
      },
    },
  },
  plugins: [],
};
