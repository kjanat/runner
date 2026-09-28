// Whole site is static: no per-request data anywhere. Enforced by
// svelte.config.js prerender (handleHttpError/handleMissingId: "fail").
export const prerender = true;
export const trailingSlash = "always";
