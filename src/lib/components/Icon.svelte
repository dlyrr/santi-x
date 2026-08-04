<script module lang="ts">
  // Every glyph is expressed as a list of `d` strings so the whole set renders
  // through real <path> nodes — no {@html}, no namespace surprises inside <svg>.
  const PATHS = {
    region: ["M6 2v14a2 2 0 0 0 2 2h14", "M18 22V8a2 2 0 0 0-2-2H2"],
    crop: ["M6 2v14a2 2 0 0 0 2 2h14", "M18 22V8a2 2 0 0 0-2-2H2"],
    monitor: [
      "M4 3h16a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z",
      "M8 21h8",
      "M12 16v5"
    ],
    window: [
      "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z",
      "M3 8.5h18",
      "M6.2 5.8h.01",
      "M8.9 5.8h.01"
    ],
    camera: [
      "M9.5 4h5L17 7h3a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2h3l2.5-3z",
      "M12 10.5a3.25 3.25 0 1 0 0 6.5 3.25 3.25 0 0 0 0-6.5z"
    ],
    fullscreen: [
      "M8 3H5a2 2 0 0 0-2 2v3",
      "M16 3h3a2 2 0 0 1 2 2v3",
      "M8 21H5a2 2 0 0 1-2-2v-3",
      "M16 21h3a2 2 0 0 0 2-2v-3"
    ],
    clock: ["M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z", "M12 7.5V12l3.2 1.9"],
    history: ["M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z", "M12 7.5V12l3.2 1.9"],
    settings: [
      "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z",
      "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z"
    ],
    copy: [
      "M11 9h9a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2v-9a2 2 0 0 1 2-2z",
      "M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"
    ],
    clipboard: [
      "M9 3h6a1 1 0 0 1 1 1v1H8V4a1 1 0 0 1 1-1z",
      "M8 5H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-2"
    ],
    folder: [
      "M4 4h4.2a2 2 0 0 1 1.6.8l1.2 1.6a2 2 0 0 0 1.6.8H20a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z"
    ],
    trash: [
      "M3 6h18",
      "M8 6V4.5A1.5 1.5 0 0 1 9.5 3h5A1.5 1.5 0 0 1 16 4.5V6",
      "M18.5 6v13.5a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2V6",
      "M10 10.5v7",
      "M14 10.5v7"
    ],
    "external-link": [
      "M15 3h6v6",
      "M10 14 21 3",
      "M21 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5"
    ],
    check: ["M20 6 9 17l-5-5"],
    x: ["M18 6 6 18", "M6 6l12 12"],
    alert: [
      "M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z",
      "M12 9v4.5",
      "M12 17.5h.01"
    ],
    info: ["M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z", "M12 11v5.2", "M12 7.8h.01"],
    "chevron-right": ["m9 18 6-6-6-6"],
    "chevron-left": ["m15 18-6-6 6-6"],
    "chevron-down": ["m6 9 6 6 6-6"],
    image: [
      "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z",
      "M9 7.4a1.7 1.7 0 1 0 0 3.4 1.7 1.7 0 0 0 0-3.4z",
      "m21 15.5-4.6-4.6a2 2 0 0 0-2.8 0L3.6 20.4"
    ],
    refresh: ["M21 12a9 9 0 1 1-2.6-6.4", "M21 3v5.5h-5.5"],
    search: ["M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14z", "m20.5 20.5-4.6-4.6"],
    download: ["M12 3v11", "m7.5 10 4.5 4.5 4.5-4.5", "M4 20h16"],
    keyboard: [
      "M4 6h16a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2z",
      "M7 10h.01",
      "M11 10h.01",
      "M15 10h.01",
      "M8 14h8"
    ],
    plus: ["M12 5v14", "M5 12h14"],
    sun: [
      "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z",
      "M12 2v2",
      "M12 20v2",
      "M4.9 4.9l1.4 1.4",
      "M17.7 17.7l1.4 1.4",
      "M2 12h2",
      "M20 12h2",
      "M4.9 19.1l1.4-1.4",
      "M17.7 6.3l1.4-1.4"
    ],
    moon: ["M20.5 14.3A8.5 8.5 0 0 1 9.7 3.5a8.5 8.5 0 1 0 10.8 10.8z"],

    /* Annotation editor (M2). The `crop` glyph above is reused by the crop
       tool — its paths are already the two-corner crop mark. */
    cursor: [
      "M4.6 3.4a.6.6 0 0 0-.8.8l6.1 15.6a.6.6 0 0 0 1.1.1l2.1-5.2a1.6 1.6 0 0 1 .9-.9l5.2-2.1a.6.6 0 0 0-.1-1.1z"
    ],
    arrow: ["M5 19 19 5", "M10 5h9v9"],
    square: [
      "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z"
    ],
    circle: ["M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z"],
    line: ["M5 19 19 5"],
    pen: [
      "M20.9 6.6a1.9 1.9 0 0 0-2.7-2.7L4.5 17.6a1.4 1.4 0 0 0-.35.6l-1.1 3.6 3.6-1.1a1.4 1.4 0 0 0 .6-.35z",
      "m15 5 4 4"
    ],
    text: ["M5 6.5V4.5h14v2", "M12 4.5v15", "M9 19.5h6"],
    highlighter: [
      "m9 11.5-5.5 5.5v3.5h9l2.5-2.5",
      "m21.5 12.2-4.4 4.4a1.8 1.8 0 0 1-2.5 0l-4.7-4.7a1.8 1.8 0 0 1 0-2.5l4.4-4.4z"
    ],
    droplet: [
      "M12 21.5a6.8 6.8 0 0 0 6.8-6.8c0-2-1-3.9-2.9-5.5S12.5 5.4 12 2.9c-.5 2.5-2 4.7-3.9 6.3S5.2 12.7 5.2 14.7A6.8 6.8 0 0 0 12 21.5z"
    ],
    /* Smart eraser (M2.11): a tilted eraser block with its ferrule band, plus a
       sparkle for the "smart" part. The block sits low-left and the sparkle
       high-right so the silhouette does not read as `highlighter`, whose nib
       occupies the opposite diagonal. */
    eraser: [
      "M3.42 14.08 10.78 6.72a1.3 1.3 0 0 1 1.84 0l3.06 3.06a1.3 1.3 0 0 1 0 1.84l-7.36 7.36a1.3 1.3 0 0 1-1.84 0l-3.06-3.06a1.3 1.3 0 0 1 0-1.84z",
      "m8.6 9.15 4.65 4.65",
      "M19.2 2.9 19.92 5.08 22.1 5.8 19.92 6.52 19.2 8.7 18.48 6.52 16.3 5.8 18.48 5.08z"
    ],
    step: [
      "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z",
      "m10.4 9.8 2.4-1.4V16",
      "M10.4 16h4.6"
    ],
    undo: ["M9 14 4 9l5-5", "M4 9h10.5a5.5 5.5 0 0 1 0 11H11"],
    redo: ["m15 14 5-5-5-5", "M20 9H9.5a5.5 5.5 0 0 0 0 11H13"],
    "zoom-in": [
      "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14z",
      "m20.5 20.5-4.6-4.6",
      "M11 8.2v5.6",
      "M8.2 11h5.6"
    ],
    "zoom-out": [
      "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14z",
      "m20.5 20.5-4.6-4.6",
      "M8.2 11h5.6"
    ],

    /* ShareX shell menu (M2.7 §4). One glyph per menu row and per Tools entry,
       including the not-yet-built ones — a disabled row still needs its icon. */
    upload: ["M12 20.5V9.5", "m7.5 14 4.5-4.5 4.5 4.5", "M4 4h16"],
    workflow: [
      "M4.5 3h4A1.5 1.5 0 0 1 10 4.5v3A1.5 1.5 0 0 1 8.5 9h-4A1.5 1.5 0 0 1 3 7.5v-3A1.5 1.5 0 0 1 4.5 3z",
      "M15.5 15h4a1.5 1.5 0 0 1 1.5 1.5v3a1.5 1.5 0 0 1-1.5 1.5h-4a1.5 1.5 0 0 1-1.5-1.5v-3a1.5 1.5 0 0 1 1.5-1.5z",
      "M10 6h4.5a2.5 2.5 0 0 1 2.5 2.5V15"
    ],
    wrench: [
      "M14.6 6.4a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.7-3.7a6 6 0 0 1-7.9 7.9l-6.9 6.9a2.1 2.1 0 0 1-3-3l6.9-6.9a6 6 0 0 1 7.9-7.9z"
    ],
    "list-check": ["m3 6.5 1.8 1.8L8 5", "m3 15.5 1.8 1.8L8 14", "M11 7h10", "M11 16h10"],
    cloud: ["M17.5 19H9a7 7 0 1 1 6.7-9h1.8a4.5 4.5 0 0 1 0 9z"],
    video: [
      "m22 8.5-5 3.5 5 3.5v-7z",
      "M13.5 6H4a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h9.5a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2z"
    ],
    scan: [
      "M3 7.5V5.5A2.5 2.5 0 0 1 5.5 3h2",
      "M16.5 3h2A2.5 2.5 0 0 1 21 5.5v2",
      "M21 16.5v2a2.5 2.5 0 0 1-2.5 2.5h-2",
      "M7.5 21h-2A2.5 2.5 0 0 1 3 18.5v-2",
      "M7 9h10",
      "M7 12.5h10",
      "M7 16h6"
    ],
    scroll: [
      "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z",
      "M12 7.5v9",
      "m8.5 13 3.5 3.5 3.5-3.5"
    ]
  } as const satisfies Record<string, readonly string[]>;

  export type IconName = keyof typeof PATHS;
</script>

<script lang="ts">
  let {
    name,
    size = 16,
    class: klass = ""
  }: { name: IconName; size?: number; class?: string } = $props();
</script>

<svg
  class={klass}
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.5"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
>
  {#each PATHS[name] as d, i (i)}
    <path {d} />
  {/each}
</svg>

<style>
  svg {
    display: block;
    flex: none;
  }
</style>
