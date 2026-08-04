// Tauri has no Node server, and adapter-static runs this app as an SPA:
// nothing is rendered or prerendered ahead of time.
export const ssr = false;
export const prerender = false;
